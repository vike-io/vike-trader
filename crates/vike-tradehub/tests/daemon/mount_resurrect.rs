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

use vike_execution::MountViewKind;
use vike_tradehub::config::ProfileDaemon;
use vike_tradehub::mount_factory::{resurrect_runtime_mounts, strategy_factory};
use vike_tradehub_core::{
    MountBuild, StrategyCorePaperConfig, StrategyCorePaperHalt, build_paper_strategy_core_with,
};

use crate::strategy_files;
use crate::support::own_sentinel;
use std::time::Duration;
use vike_log::poll::wait_until;

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
/// `crates/vike-tradehub/tests/support/temp_project.rs`'s `temp_dir`.
fn state_dir(tag: &str) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike-tradehub-mount-resurrect-{tag}-"))
        .tempdir()
        .expect("temp state root");
    let path = root.path().join("state");
    (root, path)
}

/// Resolve the ONE base profile mount the way `main.rs`'s paper arm does (the
/// `support::resolve_mounts` idiom, single row), its file read from `strategies`.
fn base_mount(strategies: &Path) -> MountBuild {
    let profile = ProfileDaemon::from_toml_str(
        r#"
venue = "sim"
symbol = "RESUR_TOK"
interval = "1m"
interval_ms = 60000

[strategy]
file = "buy_hold.rhai"
"#,
    )
    .expect("the base profile validates");
    let rows = profile.mount_rows();
    let row = &rows[0];
    let spec = row.to_mount_spec();
    let strategy = row.resolve_strategy(strategies, false).expect("a validated row resolves");
    MountBuild { strategy, spec }
}

/// The daemon's paper `StrategyCorePaperConfig` shape with the two B5 knobs armed — the state dir and the
/// SAME `mount_factory` resolver `main.rs` injects, over the strategies folder `strategies`.
///
/// ⚠ Returns the sentinel's owning `TempDir` alongside the options, and the caller must BIND it:
/// the mount consults the pinned path on every opening order, so the guard has to outlive the
/// core it is handed to. The caller binds `strategies`' own guard for the same reason: the factory
/// reads a runtime mount's file when the mount arrives.
fn opts(dir: &Path, tag: &str, strategies: &Path) -> (tempfile::TempDir, StrategyCorePaperConfig) {
    // A HALT sentinel this test owns and never creates, in a SEPARATE root from `dir` on purpose:
    // the sentinel must never appear inside the state directory the resurrect driver reads.
    let (halt_root, sentinel) = own_sentinel("mount-resurrect-halt", tag);
    assert!(!sentinel.exists(), "the pinned sentinel must not exist: {}", sentinel.display());
    let opts = StrategyCorePaperConfig {
        state_dir: Some(dir.to_path_buf()),
        strategy_factory: Some(strategy_factory(Some(strategies.to_path_buf()), false)),
        halt: StrategyCorePaperHalt::Pinned(sentinel),
        ..Default::default()
    };
    (halt_root, opts)
}

/// The RUNTIME mount's own symbol — deliberately NOT the profile mount's `RESUR_TOK`, so the two
/// are DISTINGUISHABLE in the published `MountView` rows (which carry venue/symbol/interval, not
/// the mount id). Mounting it is legitimate: the mount arm's venue check asks only that the core
/// runs an engine for `sim`, never that the symbol has its own paper book.
const RUNTIME_SYMBOL: &str = "RESUR_RT";

/// A runtime `MountSpec` on the base core's own venue, the `buy_hold.rhai` file — resolvable by the
/// real factory, distinct identity via `controller_id`.
fn runtime_spec(cid: &str) -> vike_execution::MountSpec {
    vike_execution::MountSpec {
        venue: "sim".into(),
        symbol: RUNTIME_SYMBOL.into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some(cid.to_string()),
        file: Some("buy_hold.rhai".into()),
        name: None,
        rhai: None,
        params: vike_model::params::Params::new(),
        order_client_id_code: None,
    }
}

fn live_mount_rows(handle: &vike_core::CoreHandle) -> usize {
    handle.snapshot().mounts.iter().filter(|m| m.kind == MountViewKind::Mount).count()
}

/// Is a LIVE mount row for `symbol` published? (A tombstoned slot is skipped by `mount_views`,
/// so this answers "is that mount live right now", which is what every step below asks.)
fn has_mount_row(handle: &vike_core::CoreHandle, symbol: &str) -> bool {
    handle.snapshot().mounts.iter().any(|m| m.kind == MountViewKind::Mount && m.symbol == symbol)
}

/// **The residual, closed at daemon level.** A strategy mounted AT RUNTIME onto the daemon's own
/// paper core survives a restart: core #1 records it in the topology sidecar; a restart-shaped
/// core #2 gets it back through `resurrect_runtime_mounts` — the same command lane, the same
/// factory, the same refusals a wire mount faces. Unmount is the one verb that forgets: after
/// it, a later resurrect replays nothing.
#[test]
fn a_runtime_mount_survives_a_daemon_restart_and_unmount_forgets_it() {
    let (_state_root, dir) = state_dir("survives");
    let strategies = strategy_files::standard();

    // Daemon #1: the profile mount, plus one RUNTIME mount over the wire-shaped command.
    let m = base_mount(strategies.path());
    let (_halt1, opts1) = opts(&dir, "survives-1", strategies.path());
    let mount1 = build_paper_strategy_core_with(m.strategy, &m.spec, opts1);
    mount1
        .handle
        .send_command(vike_execution::Command::MountStrategy(Box::new(runtime_spec("rt-x"))));
    assert!(
        wait_until(Duration::from_secs(10), || live_mount_rows(&mount1.handle) == 2
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
    let m = base_mount(strategies.path());
    let (_halt2, opts2) = opts(&dir, "survives-2", strategies.path());
    let mount2 = build_paper_strategy_core_with(m.strategy, &m.spec, opts2);
    assert!(!has_mount_row(&mount2.handle, RUNTIME_SYMBOL), "the fresh core mounted nothing yet");

    let outcome = resurrect_runtime_mounts(&dir, |c| mount2.handle.send_command(c));
    assert_eq!((outcome.sent, outcome.skipped), (1, 0), "exactly the recorded mount was replayed");
    assert!(
        wait_until(Duration::from_secs(10), || has_mount_row(&mount2.handle, RUNTIME_SYMBOL)),
        "the runtime mount came back on the restart-shaped core: {:?}",
        mount2.handle.snapshot().mounts
    );
    assert_eq!(live_mount_rows(&mount2.handle), 2, "profile + resurrected runtime mount, no more");

    // Unmount removes the record — the next restart replays nothing.
    mount2
        .handle
        .send_command(vike_execution::Command::UnmountStrategy { controller_id: "rt-x".into() });
    assert!(
        wait_until(Duration::from_secs(10), || !has_mount_row(&mount2.handle, RUNTIME_SYMBOL)),
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

/// **A runtime mount and an unmount show in the `StrategyStatus` rows, through the real core, server and client verb.**
/// A strategy mounted while the node runs has a row that says what it is and which account it trades; unmounting the
/// BOOT mount takes that mount's row and no other's (by position, its text moved onto the runtime mount); unmounting the
/// last one leaves no row at all.
#[test]
fn a_runtime_mount_and_its_unmount_show_in_the_status_rows() {
    use crate::support::serve_on_loopback;
    use vike_hub_protocol::auth::HubKeys;
    use vike_tradehub::publish;
    use vike_tradehub_client::wire::{HubIdentityWire, MountViewWire};

    const OBSERVE_KEY: &[u8] = b"mount-resurrect-observe-key";
    let (_state_root, dir) = state_dir("status-rows");
    let strategies = strategy_files::standard();
    let m = base_mount(strategies.path());
    let (_halt, opts) = opts(&dir, "status-rows", strategies.path());
    let mount = build_paper_strategy_core_with(m.strategy, &m.spec, opts);
    let boot_id = vike_core::strategy_state::mount_id_with(None, "sim", "RESUR_TOK", "1m");
    let boot_row = MountViewWire {
        order_client_id_code: None,
        refusal_id: None,
        strategy: "buy_hold".into(),
        params: "boot params".into(),
        live: false,
        venue: String::new(),
        symbol: String::new(),
        interval: String::new(),
        mount_id: boot_id.clone(),
        typed_params: None,
        asset_class: None,
        latched: false,
        route_key: "sim".into(),
        tier: None,
        not_running: false,
    };
    let identity = HubIdentityWire {
        name: "status-rows".into(),
        strategy: "buy_hold".into(),
        params: "boot params".into(),
        live: false,
        build: "test-build".into(),
        advertise_addr: String::new(),
        priced_venues: None,
    };
    let publisher = publish::spawn_with_mounts(
        mount.handle.snapshot_cell(),
        Some(identity),
        vec![boot_row],
        Vec::new(),
    )
    .with_mount_topology_dir(Some(dir.clone()));
    let keys = HubKeys::new(OBSERVE_KEY.to_vec(), Vec::new());
    let addr = serve_on_loopback(publisher.clone(), keys, None, None, None);
    let rows = || -> Vec<(String, String, String, Option<String>)> {
        vike_tradehub_client::strategy_status(addr, OBSERVE_KEY)
            .expect("the real server answers")
            .mounts
            .into_iter()
            .map(|m| (m.mount_id, m.params, m.route_key, m.tier))
            .collect()
    };
    let paper_row = |id: &str, params: &str| {
        (id.to_string(), params.to_string(), "sim".to_string(), Some("paper".to_string()))
    };
    let runtime_params = "venue=sim symbol=RESUR_RT interval=1m :: {}";

    mount
        .handle
        .send_command(vike_execution::Command::MountStrategy(Box::new(runtime_spec("rt-x"))));
    assert!(wait_until(Duration::from_secs(10), || has_mount_row(&mount.handle, RUNTIME_SYMBOL)));
    assert_eq!(rows(), [paper_row(&boot_id, "boot params"), paper_row("rt_x", runtime_params)]);

    mount
        .handle
        .send_command(vike_execution::Command::UnmountStrategy { controller_id: boot_id.clone() });
    assert!(wait_until(Duration::from_secs(10), || !has_mount_row(&mount.handle, "RESUR_TOK")));
    assert_eq!(rows(), [paper_row("rt_x", runtime_params)], "the boot mount's row, and only that");

    mount
        .handle
        .send_command(vike_execution::Command::UnmountStrategy { controller_id: "rt-x".into() });
    assert!(wait_until(Duration::from_secs(10), || !has_mount_row(&mount.handle, RUNTIME_SYMBOL)));
    assert_eq!(rows(), Vec::new(), "nothing runs, so no row says something does");

    publisher.shutdown();
    mount.handle.shutdown_and_join();
}

/// The edge-validation split, pure over the sidecar + a collecting sink: a record the daemon's
/// own profile vocabulary refuses (a record written before strategies were files, naming a
/// registry `name` or a `rhai` path; a nothing-selected spec) is SKIPPED with a warn and COUNTED —
/// the sendable one still goes through, and nothing errors. This is the "stale record never fails a
/// boot" contract at the driver level.
#[test]
fn stale_records_skip_with_a_count_and_valid_ones_still_send() {
    let (_state_root, dir) = state_dir("stale");
    for spec in [
        runtime_spec("rt-good"),
        // A record from before strategies were files: the retired keys are refused by the daemon's
        // own validation ("is retired"), with the fix in the warn.
        vike_execution::MountSpec {
            file: None,
            name: Some("buy_hold".to_string()),
            ..runtime_spec("rt-named")
        },
        vike_execution::MountSpec {
            file: None,
            rhai: Some("user_data/strategies/rhai/x/x.rhai".to_string()),
            ..runtime_spec("rt-script")
        },
        // ...and a spec naming no file no longer says WHAT to mount.
        vike_execution::MountSpec { file: None, ..runtime_spec("rt-empty") },
    ] {
        vike_core::mount_topology::upsert(
            &dir,
            vike_core::mount_topology::MountRecord::stamped(spec, 1_700_000_000_000),
        )
        .unwrap();
    }

    let mut sent = Vec::new();
    let outcome = resurrect_runtime_mounts(&dir, |c| sent.push(c));
    assert_eq!((outcome.sent, outcome.skipped), (1, 3));
    match sent.as_slice() {
        [vike_execution::Command::MountStrategy(spec)] => {
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

/// **A profile's `guards.initial_trading_state` reaches the daemon's core through its command
/// lane** — the daemon-level half of `vike_tradehub_core`'s decision tests (which pin WHAT is sent over a
/// journal that restored this or that): the REAL paper builder `main.rs` composes, the REAL sender
/// both daemon arms call, a hand-driven `CoreHandle`. A `halted` profile turns the spawned core's
/// gate state `Halted`; a profile that never names the key sends nothing.
#[test]
fn a_halted_profile_starts_the_core_halted_and_an_active_one_sends_nothing() {
    let (_state_root, dir) = state_dir("initial-state");
    let strategies = strategy_files::standard();
    let m = base_mount(strategies.path());
    let (_halt, opts) = opts(&dir, "initial-state", strategies.path());
    let mount = build_paper_strategy_core_with(m.strategy, &m.spec, opts);

    // The default profile (no key): nothing is sent, and the sender says so.
    let quiet = vike_config::ProfileRun::from_toml_str("").expect("the empty profile loads");
    let sent = vike_tradehub_core::send_initial_trading_state(&quiet, &[], |c| {
        mount.handle.send_command(c)
    });
    assert_eq!(sent, None, "an `active` profile sends nothing");

    let halted =
        vike_config::ProfileRun::from_toml_str("[guards]\ninitial_trading_state = \"halted\"\n")
            .expect("a lone trading state is a loadable profile");
    let sent = vike_tradehub_core::send_initial_trading_state(&halted, &[], |c| {
        mount.handle.send_command(c)
    });
    assert_eq!(sent, Some(vike_model::TradingState::Halted));
    assert!(
        wait_until(Duration::from_secs(10), || mount.handle.snapshot().trading_state
            == vike_model::TradingState::Halted),
        "the core never turned Halted: {:?}",
        mount.handle.snapshot().trading_state
    );
    mount.handle.shutdown_and_join();
}
