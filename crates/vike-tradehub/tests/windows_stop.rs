//! The Windows background stop, proved against the REAL binary and a REAL detached-shape run.
//!
//! This is `sigterm_stop.rs`'s twin, and it exists for the same reason that file does: everything
//! else about the mechanism can be shown with a bool — `vike_ops::stop`'s own tests prove the
//! watcher raises the flag and clears a stale file — and none of that proves the thing an operator
//! cares about, which is that the SHIPPED daemon, started the way a background host starts it,
//! stops through its teardown when the file appears. That question is about a process and an exit
//! status, and only a spawned binary has those.
//!
//! # What is different from the unix twin, and honestly so
//!
//! On unix the proof needs no log parsing: a process with no handler for SIGTERM is KILLED by it
//! and has no exit code at all, so `code() == Some(0)` is the entire before-and-after. Windows has
//! no such tell — there is no signal, and the hard alternative (`taskkill /F`) exits 1 rather than
//! being distinguishable in kind. So the exit code is necessary and NOT sufficient here, and the
//! teardown's own line ("shut down gracefully", with no hard cap) carries the other half. Both are
//! asserted; neither is treated as the whole answer.
//!
//! # …and the regression it guards in the same run
//!
//! Before the file is created the daemon is started with **stdin at NUL** — the background-host
//! shape, and the same one systemd produces — and must still be alive and printing snapshot
//! summaries. A non-tty EOF is NOT a stop: treating it as one would exit the daemon at startup on
//! every background start. That trap is exactly why the stdio channel could never be the background
//! stop path on either platform, so a test of the fix that did not also hold the trap shut would be
//! half a test.
//!
//! Windows only, and honestly so: `vike_ops::stop::arm_stop_file` is `cfg(windows)` because unix
//! already has a better answer for every sender (`docs/ops/graceful-stop.md`'s option 3 was weighed
//! and refused there), so there is nothing on Linux for this file to drive. ⚠ It is COMPILED by
//! CI's `windows-cross` lane and RUN only on the Windows dev box — no runner in this repo is
//! Windows.

#![cfg(windows)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to wait for a line of protocol, or for the process to die after the stop file appears.
/// Generous: every assertion here is about WHETHER something happens, never how fast.
const PATIENCE: Duration = Duration::from_secs(30);

/// Kills the child if an assertion unwinds, so a failing test never leaves a trading daemon running
/// on the box.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Build the project tree a daemon will be started against, WITHOUT starting it — so a test can
/// plant something in the state directory first, which is the only honest way to fixture "a file a
/// crashed predecessor left behind".
///
/// ⚠ Returns the OWNING `TempDir`, which the caller must hold (or hand to [`Daemon::launch`], which
/// keeps it alive for the child's whole life). Dropping it removes the tree.
///
/// This used to be `env::temp_dir().join(format!("vike_tradehub_{tag}_{pid}_{nanos}"))`, torn down
/// by a hand-rolled `impl Drop for Daemon` — which ran BEFORE the field holding the child, i.e. it
/// tried to remove a live process's working directory, which on Windows cannot be removed at all.
/// So on the platform this file is the only test for, the tree was NEVER removed. The rest of the
/// argument and the numbers measured on the CI box on 2026-08-25 are in
/// `crates/vike-tradehub/src/config.rs`'s `own_script`: 44,840 leaked directories of this shape
/// under `/tmp`, plus a REUSED pid that collides across the box's two test users so
/// `create_dir_all` succeeds on somebody else's directory and the write into it fails
/// `PermissionDenied`. `tempfile` is unique by construction and self-deleting, and the guard's
/// position in [`Daemon`]'s field list puts the removal AFTER the reap.
fn prepare(tag: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .prefix(&format!("vike_tradehub_{tag}_"))
        .tempdir()
        .expect("create the temp project root");
    let dir = root.path();
    // ⚠ `settings/state` and not just `settings`: that is the rung `vike_boot::Booted::state_dir`
    // joins to, the rung the shipped unit's write grant covers, and the rung the HALT sentinel
    // resolves to — and `arm_stop_file` REFUSES a path whose parent directory is absent rather than
    // watching for a file that can never appear. A deployment has to create it too; the shipped
    // unit's settings block says so in the same breath.
    std::fs::create_dir_all(dir.join("settings").join("state")).expect("create the temp state dir");
    std::fs::write(
        dir.join("tradehub.toml"),
        // A minimal profile is just a token_id; the summary cadence is turned up so a couple of
        // protocol lines prove liveness in about a second instead of ten.
        "token_id = \"WINDOWS_STOP_TOKEN\"\n\
         [daemon]\n\
         summary_ms = 300\n\
         shutdown_deadline_ms = 5000\n",
    )
    .expect("write the daemon profile");
    root
}

/// The path the daemon will watch, built from the SAME constant the daemon joins rather than from a
/// literal — a test that spelled the name itself would keep passing after a rename while every
/// operator's tooling broke.
fn stop_file_in(dir: &std::path::Path) -> PathBuf {
    dir.join("settings").join("state").join(vike_ops::stop::STOP_FILE_NAME)
}

/// A started daemon: the child, the project root it runs in, its stdout protocol lines, everything
/// it logged to stderr, and the stop file it is watching.
struct Daemon {
    child: Reaper,
    /// The temp project root, held so its `Drop` removes the tree at the end of the test.
    /// ⚠ Declared AFTER `child`: fields drop in declaration order, so the reaper kills and reaps
    /// the daemon before its working directory is removed — and on Windows that order is not a
    /// nicety, because a running process's current directory cannot be deleted at all.
    _root: tempfile::TempDir,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
    stop_file: PathBuf,
}

impl Daemon {
    /// Start the shipped binary over the PAPER mount with stdin at NUL — the background-host shape.
    ///
    /// Every input is a temp directory: an empty `settings/` (so `VIKE_SETTINGS_DIR` resolves there
    /// rather than walking up into the developer's real credential store), its own log dir, and the
    /// file layer off. The removed-ceiling variables are cleared because a set one is a deliberate
    /// startup REFUSAL and the harness's own environment must not decide that.
    fn start(tag: &str) -> Self {
        Self::launch(prepare(tag))
    }

    /// Start against an ALREADY-PREPARED tree — see [`prepare`]. Takes the guard by VALUE: the
    /// child outlives this call, so ownership of the directory has to travel with it.
    fn launch(root: tempfile::TempDir) -> Self {
        let dir = root.path();
        let settings = dir.join("settings");
        let profile = dir.join("tradehub.toml");

        let mut child = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
            .arg("--config")
            .arg(&profile)
            .current_dir(dir)
            .stdin(Stdio::null()) // the background-host shape: EOF at once, and NOT a stop
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("VIKE_SETTINGS_DIR", &settings)
            .env("VIKE_STATE_ROOT", dir.join("state"))
            .env("VIKE_LOG_DIR", dir.join("logs"))
            .env("VIKE_LOG_FILE_LEVEL", "off")
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_LIVE")
            .env_remove("VIKE_TRADEHUB_ADDR")
            .env_remove("VIKE_RECONCILE")
            .env_remove("RUST_LOG")
            .env_remove("VIKE_LOG")
            .spawn()
            .expect("spawn vike-tradehub");

        // Both pipes are drained on their own threads. Not tidiness: an undrained pipe fills its
        // buffer and the daemon then BLOCKS writing to it, which would look exactly like a daemon
        // ignoring the stop file.
        let (tx, lines) = mpsc::channel();
        let stdout = child.stdout.take().expect("piped stdout");
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let sink = Arc::clone(&stderr);
        let err_pipe = child.stderr.take().expect("piped stderr");
        std::thread::spawn(move || {
            for line in BufReader::new(err_pipe).lines().map_while(Result::ok) {
                sink.lock().expect("stderr buffer").push_str(&line);
                sink.lock().expect("stderr buffer").push('\n');
            }
        });

        let stop_file = stop_file_in(dir);
        Daemon { child: Reaper(child), _root: root, lines, stderr, stop_file }
    }

    /// The next protocol line on stdout, or a failure naming what the daemon logged — a dead daemon
    /// closes the pipe, so this never hangs past [`PATIENCE`].
    fn next_line(&self, what: &str) -> String {
        self.lines.recv_timeout(PATIENCE).unwrap_or_else(|e| {
            panic!(
                "no {what} line from the daemon within {PATIENCE:?} ({e}); stderr:\n{}",
                self.log()
            )
        })
    }

    fn log(&self) -> String {
        self.stderr.lock().expect("stderr buffer").clone()
    }

    /// Wait until the daemon's log shows the stop file is actually armed, or fail saying so.
    ///
    /// Not decoration: it is the difference between "the stop worked" and "something else stopped
    /// the daemon at about the same time". If the daemon never armed a watcher, creating the file
    /// proves nothing at all.
    fn await_armed(&self) {
        let deadline = Instant::now() + PATIENCE;
        while Instant::now() < deadline {
            if self.log().contains("background stop armed") {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!(
            "the daemon never reported arming the background stop file, so this test would be \
             proving nothing; stderr:\n{}",
            self.log()
        );
    }

    /// Wait for exit, or fail naming what the daemon logged on the way.
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self.child.0.try_wait().expect("try_wait") {
                Some(status) => return status,
                None if Instant::now() >= deadline => panic!(
                    "the daemon was still running {PATIENCE:?} after the stop file appeared — the \
                     graceful stop did not fire; stderr:\n{}",
                    self.log()
                ),
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

// ⚠ There is deliberately no `impl Drop for Daemon` any more: `_root`'s own destructor removes the
// tree, and it does so in the RIGHT ORDER. The hand-rolled version had it backwards — a type's own
// `drop` runs BEFORE its fields', so its `remove_dir_all` fired while `child`'s reaper had not yet
// killed the daemon. On Windows, which is the only platform this file compiles for, a live
// process's working directory cannot be removed at all, so that removal always failed silently
// (`let _ =`) and the tree stayed. Field order now reaps first and removes second.

/// ⚠ **The whole of split-plane I13, end to end**: a background-shaped daemon on Windows survives
/// its stdin EOF, then a file appearing in its state directory drives the ordinary teardown and the
/// process exits through `main`.
///
/// Without this route such a daemon has NO stop at all — `install_handlers` armed a console handler
/// for a console that a detached process does not have, and the stdio word needs a TTY — so the only
/// remaining option is a hard kill, which runs none of the teardown: no cancel sweep, no strategy
/// state save, no terminal journal snapshot, resting orders left live at the venue.
#[test]
fn the_stop_file_stops_the_daemon_gracefully() {
    let mut daemon = Daemon::start("winstop");

    let ready = daemon.next_line("ready");
    assert!(ready.contains("\"kind\":\"ready\""), "the daemon must announce itself: {ready:?}");
    daemon.await_armed();

    // Liveness AFTER the stdin EOF that `Stdio::null()` delivered at startup. Two summary lines are
    // ~600 ms of a daemon that has read EOF and kept trading — the property that must not regress,
    // because treating that EOF as a stop would exit every background start at startup.
    for _ in 0..2 {
        let line = daemon.next_line("summary");
        assert!(
            line.contains("\"kind\":\"summary\""),
            "the daemon must keep trading after a non-tty EOF, printing summaries: {line:?}"
        );
    }
    assert!(
        daemon.child.0.try_wait().expect("try_wait").is_none(),
        "a non-tty EOF must NOT stop the daemon; stderr:\n{}",
        daemon.log()
    );

    std::fs::write(&daemon.stop_file, "").expect("an operator creates the stop file");
    let status = daemon.wait_for_exit();

    assert_eq!(
        status.code(),
        Some(0),
        "the daemon must exit through main, so a supervisor that restarts on failure reads an \
         operator stop as a stop. stderr:\n{}",
        daemon.log()
    );

    let log = daemon.log();
    // ⚠ On Windows the exit code alone is NOT the proof it is on unix — there is no signal to be
    // killed by, so a hard kill is a different NUMBER rather than a different KIND. This is the
    // half that says the teardown actually ran, and it DEMANDS Graceful: accepting a hard-capped
    // teardown here would accept a stop that sat out its whole budget with nothing outstanding,
    // which is a bug (`sigterm_stop.rs`'s Finding C) rather than a stop.
    assert!(
        log.contains("shut down gracefully"),
        "the stop must run the teardown and finish it, not merely end the process; stderr:\n{log}"
    );
    assert!(
        !log.contains("hard-capped"),
        "the teardown must not report a hard cap on a healthy stop; stderr:\n{log}"
    );
    assert!(!log.contains("ABORTED"), "the teardown orchestration must not panic; stderr:\n{log}");
}

/// A file left behind by a PREVIOUS run must not stop this one — proved on the real binary, because
/// the consequence is a daemon that dies seconds after every start and an operator with no idea why.
///
/// The unit test in `vike_ops::stop` proves the clearing; this proves the daemon reaches it before
/// anything can act on the file, which is a property of WHERE the arm sits in `main` rather than of
/// the function.
#[test]
fn a_leftover_stop_file_does_not_stop_the_next_start() {
    let root = prepare("winstale");
    // Exactly what a daemon killed between the stop request and its exit leaves behind.
    std::fs::write(stop_file_in(root.path()), "").expect("plant the previous run's leftover");

    // The guard travels into the daemon, which holds it until the child has been reaped.
    let mut daemon = Daemon::launch(root);
    let ready = daemon.next_line("ready");
    assert!(ready.contains("\"kind\":\"ready\""), "the daemon must announce itself: {ready:?}");
    daemon.await_armed();
    assert!(
        !daemon.stop_file.exists(),
        "the arm must have CLEARED the leftover rather than obeyed it; stderr:\n{}",
        daemon.log()
    );

    // …and it must still be trading several polls later. A daemon that obeyed the leftover would be
    // gone within `vike_ops::stop::STOP_POLL` of arming, which is the boot loop this guards.
    for _ in 0..2 {
        let line = daemon.next_line("summary");
        assert!(
            line.contains("\"kind\":\"summary\""),
            "the daemon must still be trading after a leftover stop file: {line:?}"
        );
    }
    assert!(
        daemon.child.0.try_wait().expect("try_wait").is_none(),
        "a leftover stop file must not stop a fresh start; stderr:\n{}",
        daemon.log()
    );
}
