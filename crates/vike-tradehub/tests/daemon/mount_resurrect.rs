//! Runtime mounts SURVIVE a daemon restart (split-plane B5, residual closed) — the daemon-level
//! half of the topology-as-state design, driven the way `multi_mount_profile` drives its pieces:
//! the REAL paper builder `main.rs` composes, the REAL `mount_factory` resolver, hand-driven
//! `CoreHandle` commands, no process kill (the kill shape — drop-without-teardown — is pinned by
//! `vike-core`'s white-box `runtime_mount_tests`; what this file owns is the DAEMON driver,
//! `vike_tradehub::mount_factory`'s `resurrect_runtime_mounts`, and its contract):
//!
//! - a runtime-mounted strategy on core #1 is resurrected onto a restart-shaped core #2 through
//!   the SAME `Command::MountStrategy` lane a wire mount takes;
//! - unmount removes the record, so a later resurrect replays nothing;
//! - a stale record (a spec the daemon's own validation refuses) SKIPS with a warn — counted,
//!   never an error, never a boot failure;
//! - a corrupt topology file resurrects nothing and errors nothing.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use vike_core::MountRowKind;
use vike_run::{build_paper_strategy_core_with, PaperHalt, PaperMountOpts, StrategyMountSpec};
use vike_tradehub::config::{DaemonProfile, AS_MAKER_NAMES};
use vike_tradehub::mount_factory::{resurrect_runtime_mounts, strategy_factory};

/// A fresh per-test state dir that does NOT exist yet, in
/// `crates/vike-core/src/scratch.rs`'s `Scratch::reserved` shape: the returned `TempDir` is the
/// owned ROOT, and the path points one level INTO it.
///
/// The RESERVED half is load-bearing: `vike_core::mount_topology::upsert` creates the directory
/// itself, `a_corrupt_topology_file_resurrects_nothing_and_never_errors` calls `create_dir_all`
/// before planting its file, and `mount_topology::read` has an absent-directory arm — so the code
/// under test is what must materialise this path. Owning the ROOT is what still makes the sidecar
/// the test writes there self-cleaning.
///
/// ⚠ BIND the guard for the whole test. This used to be
/// `env::temp_dir().join(format!("…-{pid}-{tag}"))` with a `remove_dir_all` in FRONT of it — a
/// PRE-clean against pid reuse, never cleanup — plus a `remove_dir_all` at the end of each test
/// that ran only when the test passed. The pre-clean is what a `tempfile` name makes unnecessary
/// (a fresh path cannot be stale) and it was also the dangerous half: it deletes a directory whose
/// name a DIFFERENT user's run may own, so it either fails silently or destroys their fixture.
/// The measured numbers for this family — 44,840 leaked directories under the CI box's `/tmp` on
/// 2026-08-25, and the two-users/one-pid `PermissionDenied` flake — are in
/// `crates/vike-tradehub/src/config.rs`'s `own_script`.
fn state_dir(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-mount-resurrect-{tag}-"))
        .tempdir()
        .expect("temp state root");
    let path = root.path().join("state");
    (root, path)
}

/// A HALT sentinel this test owns and never creates (`multi_mount_profile::own_sentinel`'s
/// pinning — a paper mount is HALT-armed by design, and this test must not inherit the box's).
///
/// ⚠ Returns the owning `TempDir` ALONGSIDE the path; the caller must BIND it. Same reserved shape
/// as [`state_dir`], and a SEPARATE root on purpose: the sentinel must never appear inside the
/// state directory the resurrect driver reads.
fn own_sentinel(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-mount-resurrect-halt-{tag}-"))
        .tempdir()
        .expect("temp sentinel root");
    let path = root.path().join("HALT");
    (root, path)
}

fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Resolve the ONE base profile mount the way `main.rs`'s paper arm does (the
/// `multi_mount_profile::resolve_mounts` idiom, single row).
fn base_mount() -> StrategyMountSpec {
    let profile = DaemonProfile::from_toml_str(
        r#"
venue = "sim"
symbol = "RESUR_TOK"
interval = "1m"
interval_ms = 60000

[strategy]
name = "buy_hold"
"#,
    )
    .expect("the base profile validates");
    let rows = profile.mount_rows();
    let row = &rows[0];
    let cfg = row.to_mount_config();
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(&cfg).expect("a validated row resolves");
    StrategyMountSpec { strategy, spec }
}

/// The daemon's paper `PaperMountOpts` shape with the two B5 knobs armed — the state dir and the
/// SAME `mount_factory` resolver `main.rs` injects.
///
/// ⚠ Returns the sentinel's owning `TempDir` alongside the options, and the caller must BIND it:
/// the mount consults the pinned path on every opening order, so the guard has to outlive the
/// core it is handed to.
fn opts(dir: &Path, tag: &str) -> (tempfile::TempDir, PaperMountOpts) {
    let (halt_root, sentinel) = own_sentinel(tag);
    assert!(!sentinel.exists(), "the pinned sentinel must not exist: {}", sentinel.display());
    let opts = PaperMountOpts {
        state_dir: Some(dir.to_path_buf()),
        strategy_factory: Some(strategy_factory()),
        halt: PaperHalt::Pinned(sentinel),
        ..Default::default()
    };
    (halt_root, opts)
}

/// The RUNTIME mount's own symbol — deliberately NOT the profile mount's `RESUR_TOK`, so the two
/// are DISTINGUISHABLE in the published `MountView` rows (which carry venue/symbol/interval, not
/// the mount id). Mounting it is legitimate: the mount arm's venue check asks only that the core
/// runs an engine for `sim`, never that the symbol has its own paper book.
const RUNTIME_SYMBOL: &str = "RESUR_RT";

/// A runtime `MountSpec` on the base core's own venue, `buy_hold` by name — resolvable by the
/// real factory, distinct identity via `controller_id`.
fn runtime_spec(cid: &str) -> vike_exec::MountSpec {
    vike_exec::MountSpec {
        venue: "sim".into(),
        symbol: RUNTIME_SYMBOL.into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some(cid.to_string()),
        name: Some("buy_hold".into()),
        rhai: None,
        params: serde_json::json!({}),
    }
}

fn live_mount_rows(handle: &vike_core::CoreHandle) -> usize {
    handle.snapshot().mounts.iter().filter(|m| m.kind == MountRowKind::Mount).count()
}

/// Is a LIVE mount row for `symbol` published? (A tombstoned slot is skipped by `mount_views`,
/// so this answers "is that mount live right now", which is what every step below asks.)
fn has_mount_row(handle: &vike_core::CoreHandle, symbol: &str) -> bool {
    handle.snapshot().mounts.iter().any(|m| m.kind == MountRowKind::Mount && m.symbol == symbol)
}

/// **The residual, closed at daemon level.** A strategy mounted AT RUNTIME onto the daemon's own
/// paper core survives a restart: core #1 records it in the topology sidecar; a restart-shaped
/// core #2 gets it back through `resurrect_runtime_mounts` — the same command lane, the same
/// factory, the same refusals a wire mount faces. Unmount is the one verb that forgets: after
/// it, a later resurrect replays nothing.
#[test]
fn a_runtime_mount_survives_a_daemon_restart_and_unmount_forgets_it() {
    let (_state_root, dir) = state_dir("survives");

    // Daemon #1: the profile mount, plus one RUNTIME mount over the wire-shaped command.
    let m = base_mount();
    let (_halt1, opts1) = opts(&dir, "survives-1");
    let mount1 = build_paper_strategy_core_with(m.strategy, &m.spec, opts1);
    mount1.handle.send_command(vike_exec::Command::MountStrategy(Box::new(runtime_spec("rt-x"))));
    assert!(
        wait_until(10, || live_mount_rows(&mount1.handle) == 2
            && has_mount_row(&mount1.handle, RUNTIME_SYMBOL)),
        "profile + runtime mount both live on core #1: {:?}",
        mount1.handle.snapshot().mounts
    );
    // A clean stop does NOT forget the runtime mount ("stop the daemon" is not "unmount") — and
    // the kill shape (drop without teardown) leaves the identical file, pinned white-box in
    // vike-core's `mount_records_topology_and_only_unmount_forgets_it`.
    mount1.handle.shutdown_and_join();
    assert_eq!(vike_core::mount_topology::read(&dir).len(), 1, "the record survived the stop");

    // Daemon #2 (the restart): same profile, fresh core — then the resurrect both daemon arms run
    // after the core spawns and before feeds arm.
    //
    // ⚠ There is deliberately no "and nothing else is mounted yet" assertion here. A freshly
    // spawned core publishes NO snapshot until its first ingest marks it dirty, so the cell still
    // holds `CoreSnapshot::empty` and its `mounts` vec is `[]` — an emptiness that says nothing
    // about the runtime mount, and which the only available cure (sending something) would
    // perturb. The load-bearing proof is the TRANSITION below: `RUNTIME_SYMBOL` has no row, the
    // resurrect is the only thing that happens, and then it does.
    let m = base_mount();
    let (_halt2, opts2) = opts(&dir, "survives-2");
    let mount2 = build_paper_strategy_core_with(m.strategy, &m.spec, opts2);
    assert!(!has_mount_row(&mount2.handle, RUNTIME_SYMBOL), "the fresh core mounted nothing yet");

    let outcome = resurrect_runtime_mounts(&dir, |c| mount2.handle.send_command(c));
    assert_eq!((outcome.sent, outcome.skipped), (1, 0), "exactly the recorded mount was replayed");
    assert!(
        wait_until(10, || has_mount_row(&mount2.handle, RUNTIME_SYMBOL)),
        "the runtime mount came back on the restart-shaped core: {:?}",
        mount2.handle.snapshot().mounts
    );
    assert_eq!(live_mount_rows(&mount2.handle), 2, "profile + resurrected runtime mount, no more");

    // Unmount removes the record — the next restart replays nothing.
    mount2
        .handle
        .send_command(vike_exec::Command::UnmountStrategy { controller_id: "rt-x".into() });
    assert!(
        wait_until(10, || !has_mount_row(&mount2.handle, RUNTIME_SYMBOL)),
        "unmounted: {:?}",
        mount2.handle.snapshot().mounts
    );
    assert!(vike_core::mount_topology::read(&dir).is_empty(), "unmount removed the record");
    let mut replayed = Vec::new();
    let outcome = resurrect_runtime_mounts(&dir, |c| replayed.push(c));
    assert_eq!((outcome.sent, outcome.skipped), (0, 0));
    assert!(replayed.is_empty());
    mount2.handle.shutdown_and_join();
    // No hand cleanup: `_state_root` drops here and removes the sidecar with its root.
}

/// The edge-validation split, pure over the sidecar + a collecting sink: a record the daemon's
/// own profile vocabulary refuses (an A-S maker name; a nothing-selected spec) is SKIPPED with a
/// warn and COUNTED — the sendable one still goes through, and nothing errors. This is the
/// "stale record never fails a boot" contract at the driver level.
#[test]
fn stale_records_skip_with_a_count_and_valid_ones_still_send() {
    let (_state_root, dir) = state_dir("stale");
    for spec in [
        runtime_spec("rt-good"),
        // The A-S maker names are runtime-refused by the daemon's own validation.
        vike_exec::MountSpec {
            name: Some(AS_MAKER_NAMES[0].to_string()),
            ..runtime_spec("rt-maker")
        },
        // ...and a spec naming neither `name` nor `rhai` no longer says WHAT to mount.
        vike_exec::MountSpec { name: None, rhai: None, ..runtime_spec("rt-empty") },
    ] {
        vike_core::mount_topology::upsert(
            &dir,
            vike_core::mount_topology::MountRecord::stamped(spec, 1_700_000_000_000),
        )
        .unwrap();
    }

    let mut sent = Vec::new();
    let outcome = resurrect_runtime_mounts(&dir, |c| sent.push(c));
    assert_eq!((outcome.sent, outcome.skipped), (1, 2));
    match sent.as_slice() {
        [vike_exec::Command::MountStrategy(spec)] => {
            assert_eq!(spec.controller_id.as_deref(), Some("rt-good"));
        }
        other => panic!("exactly the valid record is sent, as MountStrategy: {other:?}"),
    }
    // No hand cleanup: `_state_root` drops here and removes the sidecar with its root.
}

/// A corrupt topology file resurrects NOTHING and errors NOTHING — `mount_topology::read`'s
/// loud-but-fail-open contract carried through the driver, so a mangled sidecar can never stop
/// the daemon from booting.
#[test]
fn a_corrupt_topology_file_resurrects_nothing_and_never_errors() {
    let (_state_root, dir) = state_dir("corrupt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(vike_core::mount_topology::topology_path(&dir), b"] definitely not json {")
        .unwrap();

    let mut sent = Vec::new();
    let outcome = resurrect_runtime_mounts(&dir, |c| sent.push(c));
    assert_eq!((outcome.sent, outcome.skipped), (0, 0), "skip-all, loudly logged, no error");
    assert!(sent.is_empty());
    // No hand cleanup: `_state_root` drops here and removes the corrupt file with its root.
}
