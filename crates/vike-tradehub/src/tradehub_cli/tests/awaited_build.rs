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
use crate::tradehub_cli::awaited_builds::{AwaitedMount, Opener, awaiting_ids, start_with};
use crate::tradehub_cli::profile::resolve_mounts;

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
    assert_eq!(builds.get(MOUNT_ID), Some(Err(BUILDING.to_string())), "its row says building");
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

#[test]
fn a_failed_build_leaves_the_mount_deactivated_with_the_diagnostic_and_the_node_runs() {
    let files = crate::test_strategy_files::folder(&[(FILE, MAKER)]);
    let resolved = resolved_maker(&files);
    let awaited = AwaitedMount::of_resolved(&resolved, false);
    let handle = paper_core(resolved);
    const DIAGNOSTIC: &str = "strategy file market_making/maker.rs: NOT RUNNING: its build failed: \
                              error[E0425]: cannot find value `x` in this scope";
    let opener: Opener =
        Arc::new(|_: &SandboxOpen<'_>| Err(OpenRefusal::Refused(DIAGNOSTIC.to_string())));
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    for thread in start_with(awaited, &handle.event_sender(), &builds, &stop, &opener) {
        thread.join().expect("a refused build ends its thread, nothing else");
    }
    assert_eq!(
        builds.get(MOUNT_ID),
        Some(Err(DIAGNOSTIC.to_string())),
        "the row carries the words"
    );
    // The node runs on (its core still takes a parcel and publishes), and the mount has no slot.
    assert!(!holds_now(&handle, MOUNT_ID), "a failed build never mounts");
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
            Err(OpenRefusal::BuilderUnanswered(OLD.to_string()))
        } else {
            Ok(opened())
        }
    });
    let builds = MountBuilds::default();
    let stop = Arc::new(AtomicBool::new(false));
    let threads = start_with(awaited, &handle.event_sender(), &builds, &stop, &opener);
    wait_until("the row says the builder did not answer", Duration::from_secs(5), || {
        builds.get(MOUNT_ID) == Some(Err(OLD.to_string()))
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
        Err(OpenRefusal::BuilderUnanswered("connect refused".to_string()))
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
