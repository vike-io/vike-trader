//! **The tradehub does not wait for a build** (the owner's ruling of 2026-11-14), the daemon's half: a `.rs`
//! mount is resolved at the start WITHOUT a build (the file, its header, its params and its sha256 judged), the
//! node runs at once with the mount deactivated `building <file>`, and the mount starts BY ITSELF when its build
//! answers (`super::super::awaited_builds`). The build and the sandboxed child are stood in for by an opener, as
//! `ProfileDaemon::resolve_mount_with`'s tests stand them in: the real ones need the builder service and Linux.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::MountResolved;
use crate::config::{MountBuilds, OpenRefusal, ProfileDaemon, SandboxOpen, SandboxOpened};
use crate::publish::spawn_with_mounts;
use crate::tradehub_cli::awaited_builds::{
    AwaitedMount, Opener, awaiting_ids, builder_ladder, start_with,
};
use crate::tradehub_cli::mount_rows::wire_mount_rows;
use crate::tradehub_cli::paper::wire_mount_seeds_for;
use crate::tradehub_cli::profile::{primary_mount_identity, resolve_mounts};
use vike_hub_protocol::frame::{BodyCodec, read_frame};
use vike_tradehub_client::proto::Response;
use vike_tradehub_client::wire::HubIdentityWire;

/// **A builder that did not answer is asked again after these pauses, verbatim**: two seconds doubling to a minute,
/// then a minute for every miss after.
#[test]
fn a_builder_that_did_not_answer_is_asked_again_after_two_seconds_doubling_to_a_minute() {
    let mut ladder = builder_ladder();
    let secs: Vec<u64> = (0..7).map(|_| ladder.failed().as_secs()).collect();
    assert_eq!(secs, [2, 4, 8, 16, 32, 60, 60]);
}

const FILE: &str = "market_making/maker.rs";
const MOUNT_ID: &str = "bybit__BTCUSDT__1m";
const BUILDING: &str = "NOT RUNNING: building market_making/maker.rs";

/// A `.rs` maker file's header (its body is never compiled here).
const MAKER: &str = "\
//! A test maker.
//!
//! ```toml
//! [strategy]
//! runs_on = \"live\"
//! why = \"a test file: its build and its process are stood in for\"
//!
//! [params.qty]
//! type = \"number\"
//! [params.half_spread]
//! type = \"number\"
//! ```
";

fn maker_profile(params: &str) -> ProfileDaemon {
    ProfileDaemon::from_toml_str(&format!(
        "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\n[strategy]\nfile = \"{FILE}\"\n\
         [strategy.params]\n{params}"
    ))
    .expect("the maker profile loads")
}

const PARAMS: &str = "qty = 0.001\nhalf_spread = 0.0005\n";

/// A stand-in for the built file's sandboxed process: it trades nothing.
struct Quiet;

impl vike_model::Strategy<vike_core::LiveBroker> for Quiet {}

fn opened() -> SandboxOpened {
    SandboxOpened {
        strategy: Box::new(Quiet),
        echo: vec![("qty".into(), "0.001".into()), ("half_spread".into(), "0.0005".into())],
        remote: None,
        sdk: None,
    }
}

/// The daemon's paper arm in small: the production paper builder over the one resolved mount, the mounts
/// waiting for their build parked.
fn paper_core(resolved: Vec<MountResolved>) -> vike_core::CoreHandle {
    let ids = awaiting_ids(&resolved);
    let m = resolved.into_iter().next().expect("one mount");
    // A sentinel this test owns and never creates: the paper book's HALT stays off.
    let sentinel = std::env::temp_dir()
        .join(format!("vike-tradehub-awaited-build-{}", std::process::id()))
        .join("HALT");
    let opts = vike_tradehub_core::StrategyCorePaperConfig {
        halt: vike_tradehub_core::StrategyCorePaperHalt::Pinned(sentinel),
        mounts_awaiting_strategy: ids,
        ..Default::default()
    };
    vike_tradehub_core::build_paper_strategy_core_with(m.strategy, &m.spec, opts).handle
}

fn wait_until(what: &str, within: Duration, probe: impl Fn() -> bool) {
    let deadline = Instant::now() + within;
    while !probe() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn holds(handle: &vike_core::CoreHandle, id: &str) -> bool {
    handle.snapshot_cell().load().mounts.iter().any(|m| m.mount_id == id)
}

/// Whether the core holds mount `id` NOW: an idle core publishes on a change, so an EMPTY parcel of mounts (it hands
/// the core nothing) makes it publish, and the frame after it answers. It also proves the core still runs.
fn holds_now(handle: &vike_core::CoreHandle, id: &str) -> bool {
    let before = handle.snapshot_cell().load().sequence;
    handle
        .event_sender()
        .add_account_mounts(Box::new(Vec::<vike_core::Mount>::new()))
        .expect("the core runs");
    wait_until("the core publishes", Duration::from_secs(10), || {
        handle.snapshot_cell().load().sequence > before
    });
    holds(handle, id)
}

/// The resolve at the start: the `.rs` file judged, not built, its file and sha256 recorded.
fn resolved_maker(files: &tempfile::TempDir) -> Vec<MountResolved> {
    let resolved = resolve_mounts(&maker_profile(PARAMS), false, Some(files.path()), false)
        .expect("a .rs mount resolves at the start without a build");
    assert!(resolved[0].building.is_some(), "the .rs file waits for its build");
    assert_eq!(resolved[0].strategy_params, BUILDING);
    let file = resolved[0].spec.strategy_file.clone().expect("its file and sha256 are recorded");
    assert_eq!(file.path, FILE);
    assert_eq!(file.sha256, vike_hub_protocol::hash::sha256_hex(MAKER.as_bytes()));
    resolved
}

#[test]
fn a_rs_mount_waits_for_its_build_while_the_node_runs_and_then_starts_by_itself() {
    let files = crate::test_strategy_files::folder(&[(FILE, MAKER)]);
    let resolved = resolved_maker(&files);
    let awaited = AwaitedMount::of_resolved(&resolved, false);
    let handle = paper_core(resolved);
    // The node is up at once: its core runs and publishes, and holds no slot for the mount.
    assert!(!holds_now(&handle, MOUNT_ID), "the mount does not run before its build");

    // The build: held until the test releases it.
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let opener: Opener = Arc::new(move |request: &SandboxOpen<'_>| {
        assert_eq!((request.file, request.sha256), (FILE, sha_of_maker().as_str()));
        gate.lock().expect("the gate").recv().expect("the test releases the build");
        Ok(opened())
    });
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    let threads = start_with(awaited, &handle.event_sender(), &builds, &stop, &opener);
    assert_eq!(builds.get(MOUNT_ID), Some(Err(BUILDING.into())), "its row says building, no id");
    std::thread::sleep(Duration::from_millis(200));
    assert!(!holds_now(&handle, MOUNT_ID), "no slot while it builds");

    release.send(()).expect("the build thread waits");
    for thread in threads {
        thread.join().expect("the build thread ends");
    }
    wait_until("the mount starts by itself", Duration::from_secs(10), || holds(&handle, MOUNT_ID));
    assert_eq!(builds.get(MOUNT_ID), Some(Ok("qty=0.001 half_spread=0.0005".to_string())));
    handle.shutdown_and_join();
}

fn sha_of_maker() -> String {
    vike_hub_protocol::hash::sha256_hex(MAKER.as_bytes())
}

/// **A mount whose build answered no longer reads NOT RUNNING where the node publishes its status** (the CI box,
/// 2026-11-20: `trade status` printed `params: NOT RUNNING: building market_making/spread_maker.rs` ten hours after
/// that mount was BUILT and trading). The boot composes the identity block's params line from its rows while the file
/// builds; the build answering brings the identity (the `StrategyStatus` reply's `effective_params`), the pushed
/// frame's identity and the row up to date (`crate::publish::PublisherHandle::with_mount_builds`).
#[test]
fn a_built_mount_no_longer_reads_not_running_in_the_published_status() {
    let files = crate::test_strategy_files::folder(&[(FILE, MAKER)]);
    let resolved = resolved_maker(&files);
    let awaited = AwaitedMount::of_resolved(&resolved, false);
    // The boot's own composition of the identity block's params line and of the rows (`tradehub_cli::run`).
    let (_, strategy, params) = primary_mount_identity(&maker_profile(PARAMS), &resolved, false);
    let rows = wire_mount_rows(
        wire_mount_seeds_for(&resolved, false, &strategy, &params),
        &std::collections::HashSet::new(),
    );
    let identity = HubIdentityWire {
        name: "maker".into(),
        strategy,
        params,
        live: false,
        build: String::new(),
        advertise_addr: String::new(),
        priced_venues: None,
    };
    let handle = paper_core(resolved);
    let builds = MountBuilds::default();
    let publisher = spawn_with_mounts(handle.snapshot_cell(), Some(identity), rows, Vec::new());
    let publisher = publisher.with_mount_builds(builds.clone());
    let published = || {
        let bytes = publisher.snapshot_frame_now(BodyCodec::Json).to_vec();
        let read = read_frame::<_, Response>(&mut std::io::Cursor::new(bytes));
        let Ok(Response::SnapshotFrame(wire)) = read else {
            panic!("expected a SnapshotFrame");
        };
        let row = publisher.mount_rows_now(publisher.mounts()).remove(0);
        let status = publisher.identity().expect("an identity").params;
        (status, wire.identity.expect("the frame's identity").params, row.not_running, row.params)
    };

    let (release, gate) = std::sync::mpsc::channel::<()>();
    let gate = Mutex::new(gate);
    let opener: Opener = Arc::new(move |_: &SandboxOpen<'_>| {
        gate.lock().expect("the gate").recv().expect("the test releases the build");
        Ok(opened())
    });
    let stop = Arc::new(AtomicBool::new(false));
    let threads = start_with(awaited, &handle.event_sender(), &builds, &stop, &opener);
    assert_eq!(
        published(),
        (BUILDING.to_string(), BUILDING.to_string(), true, BUILDING.to_string()),
        "while it builds, every published line says so"
    );
    let building = publisher.identity_made().expect("the line made for the build's first answer");

    release.send(()).expect("the build thread waits");
    for thread in threads {
        thread.join().expect("the build thread ends");
    }
    wait_until("the mount starts by itself", Duration::from_secs(10), || holds(&handle, MOUNT_ID));
    let running = "qty=0.001 half_spread=0.0005".to_string();
    assert_eq!(
        published(),
        (running.clone(), running.clone(), false, running.clone()),
        "once it runs, no published line says NOT RUNNING"
    );
    let made = publisher.identity_made().expect("the line made for the build's answer");
    assert!(!Arc::ptr_eq(&building, &made), "an answer makes the line again");
    // A publish with no new answer reuses the line: a frame makes none (the CI box publishes ~100 a second).
    assert_eq!(published(), (running.clone(), running.clone(), false, running));
    let again = publisher.identity_made().expect("the line is kept");
    assert!(Arc::ptr_eq(&made, &again), "no answer came, so the line was not made again");
    publisher.shutdown();
    handle.shutdown_and_join();
}

/// The boot's mount rows, composed as the boot composes them (`tradehub_cli::run`).
fn boot_rows(resolved: &[MountResolved]) -> Vec<vike_tradehub_client::wire::MountViewWire> {
    let (_, strategy, params) = primary_mount_identity(&maker_profile(PARAMS), resolved, false);
    wire_mount_rows(
        wire_mount_seeds_for(resolved, false, &strategy, &params),
        &std::collections::HashSet::new(),
    )
}

/// The mount's row as the node publishes it now (`crate::publish::PublisherHandle::mount_rows_now` over `builds`).
fn published_row(
    rows: Vec<vike_tradehub_client::wire::MountViewWire>,
    handle: &vike_core::CoreHandle,
    builds: &MountBuilds,
) -> vike_tradehub_client::wire::MountViewWire {
    let publisher = spawn_with_mounts(handle.snapshot_cell(), None, rows, Vec::new())
        .with_mount_builds(builds.clone());
    let row = publisher.mount_rows_now(publisher.mounts()).remove(0);
    publisher.shutdown();
    row
}

/// **A failed build leaves the mount deactivated, its row holding the refusal under ONE id, the builder's**: the
/// compile refusal crosses `OpenRefusal` inside the profile's carrier (`MountProfileRefusal::BuildFailed`, which shows
/// the id it carries), and the row's `refusal_id` is that id beside the bare words.
#[test]
fn a_failed_build_leaves_the_mount_deactivated_with_the_diagnostic_and_the_node_runs() {
    let files = crate::test_strategy_files::folder(&[(FILE, MAKER)]);
    let resolved = resolved_maker(&files);
    let awaited = AwaitedMount::of_resolved(&resolved, false);
    let rows = boot_rows(&resolved);
    let handle = paper_core(resolved);
    const DIAGNOSTIC: &str = "strategy file market_making/maker.rs: NOT RUNNING: its build failed: \
                              error[E0425]: cannot find value `x` in this scope";
    // What `crate::config::open_built` makes of the builder's compile refusal (a stand-in for its frame).
    let refused = crate::refusals::MountProfileRefusal::BuildFailed {
        file: FILE.to_string(),
        why: vike_utility::errors::CarriedRefusal::new(
            Some("strategy.build.compile_failed".into()),
            "error[E0425]: cannot find value `x` in this scope".into(),
        ),
    }
    .carried();
    assert_eq!(refused.to_string(), DIAGNOSTIC);
    let opener: Opener =
        Arc::new(move |_: &SandboxOpen<'_>| Err(OpenRefusal::Refused(refused.clone())));
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    for thread in start_with(awaited, &handle.event_sender(), &builds, &stop, &opener) {
        thread.join().expect("a refused build ends its thread, nothing else");
    }
    let row = published_row(rows, &handle, &builds);
    assert!(row.not_running, "the row says it does not run");
    assert_eq!(row.params, DIAGNOSTIC, "the row carries the bare words");
    assert_eq!(
        row.refusal_id.as_deref(),
        Some("strategy.build.compile_failed"),
        "the builder's id"
    );
    assert_eq!(row.shown_params(), format!("error [strategy.build.compile_failed]: {DIAGNOSTIC}"));
    // The node runs on (its core still takes a parcel and publishes), and the mount has no slot.
    assert!(!holds_now(&handle, MOUNT_ID), "a failed build never mounts");
    handle.shutdown_and_join();
}

/// **The daemon's own opener refuses a `.rs` file in a process started without a builder, and the row shows its id**
/// (`crate::config::open_built`, the production path, no stand-in): `tradehub.profile.strategy.no_builder`, once.
#[test]
fn a_file_with_no_builder_reads_its_refusal_id_on_its_row() {
    let files = crate::test_strategy_files::folder(&[(FILE, MAKER)]);
    let resolved = resolved_maker(&files);
    let awaited = AwaitedMount::of_resolved(&resolved, false);
    let rows = boot_rows(&resolved);
    let handle = paper_core(resolved);
    // No builder was ever set in this process (`crate::config::use_builder`).
    let opener: Opener = Arc::new(crate::config::open_built);
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    for thread in start_with(awaited, &handle.event_sender(), &builds, &stop, &opener) {
        thread.join().expect("a refused open ends its thread");
    }
    let row = published_row(rows, &handle, &builds);
    assert_eq!(row.refusal_id.as_deref(), Some("tradehub.profile.strategy.no_builder"));
    let shown = row.shown_params();
    assert_eq!(shown.matches("error [").count(), 1, "ONE id: {shown}");
    assert!(
        shown.starts_with(
            "error [tradehub.profile.strategy.no_builder]: strategy file market_making/maker.rs: a `.rs` strategy \
             file runs in a sandboxed process of its own"
        ),
        "{shown}"
    );
    handle.shutdown_and_join();
}

/// **The pre-flight's path**: the start judges the file without asking any builder, so a builder that is not
/// there or speaks an older protocol cannot fail the boot. What the pre-flight must still refuse (a missing file,
/// a header that does not parse, a key the header does not declare) is refused at the same resolve; and a builder
/// that does not answer is asked again, the node running meanwhile, until it builds.
#[test]
fn an_old_or_absent_builder_fails_no_start_and_is_asked_again() {
    let files = crate::test_strategy_files::folder(&[
        (FILE, MAKER),
        ("market_making/broken.rs", "//! ```toml\n//! [strategy\n//! ```\n"),
    ]);
    // No builder was ever set in this process (`crate::config::use_builder`): the resolve never asks one.
    let resolved = resolved_maker(&files);
    let refused = |profile: ProfileDaemon| {
        resolve_mounts(&profile, false, Some(files.path()), false).err().is_some()
    };
    let missing = ProfileDaemon::from_toml_str(
        "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\n[strategy]\nfile = \"market_making/none.rs\"\n",
    )
    .expect("loads");
    assert!(refused(missing), "a file that is not there refuses the start");
    let broken = ProfileDaemon::from_toml_str(
        "venue = \"bybit\"\nsymbol = \"BTCUSDT\"\n[strategy]\nfile = \"market_making/broken.rs\"\n",
    )
    .expect("loads");
    assert!(refused(broken), "a header that does not parse refuses the start");
    assert!(refused(maker_profile("qtyy = 0.001\n")), "an undeclared key refuses the start");

    let awaited = AwaitedMount::of_resolved(&resolved, false);
    let handle = paper_core(resolved);
    const OLD: &str = "strategy file market_making/maker.rs: NOT RUNNING: building, and the strategy builder \
                       did not answer: strategy-builder protocol error: protocol version mismatch: this client \
                       speaks 2, the builder speaks 1";
    let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&asked);
    let opener: Opener = Arc::new(move |_: &SandboxOpen<'_>| {
        if counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            Err(OpenRefusal::BuilderUnanswered(OLD.into()))
        } else {
            Ok(opened())
        }
    });
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    let threads = start_with(awaited, &handle.event_sender(), &builds, &stop, &opener);
    wait_until("the row says the builder did not answer", Duration::from_secs(5), || {
        builds.get(MOUNT_ID) == Some(Err(OLD.into()))
    });
    assert!(!holds_now(&handle, MOUNT_ID), "the node runs while the builder does not answer");
    for thread in threads {
        thread.join().expect("the build thread ends once it built");
    }
    assert_eq!(asked.load(std::sync::atomic::Ordering::SeqCst), 2, "asked again, once");
    wait_until("the mount starts once the builder answered", Duration::from_secs(10), || {
        holds(&handle, MOUNT_ID)
    });
    handle.shutdown_and_join();
}

/// **A stop ends the wait**: a build thread waiting to ask an unanswering builder again ends at the stop, so a
/// restart's shutdown waits on no build.
#[test]
fn a_stop_ends_a_build_thread_waiting_for_its_builder() {
    let files = crate::test_strategy_files::folder(&[(FILE, MAKER)]);
    let resolved = resolved_maker(&files);
    let awaited = AwaitedMount::of_resolved(&resolved, false);
    let handle = paper_core(resolved);
    let opener: Opener = Arc::new(|_: &SandboxOpen<'_>| {
        Err(OpenRefusal::BuilderUnanswered("connect refused".into()))
    });
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    let threads = start_with(awaited, &handle.event_sender(), &builds, &stop, &opener);
    wait_until("the first ask", Duration::from_secs(5), || builds.get(MOUNT_ID).is_some());
    let stopped = Instant::now();
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    for thread in threads {
        thread.join().expect("the build thread ends at the stop");
    }
    assert!(stopped.elapsed() < Duration::from_secs(1), "{:?}", stopped.elapsed());
    handle.shutdown_and_join();
}
