//! The graceful stop, proved against the REAL binary and a REAL signal.
//!
//! Everything else about this change can be shown with a bool: `vike_ops::stop`'s tests prove the
//! flag is exactly-once under concurrency, and `main.rs`'s own tests prove the control channel's
//! EOF rule. None of that proves the thing an operator actually cares about — that `systemctl stop`
//! now runs the teardown — because that question is about a process, a signal disposition and an
//! exit status, and only a spawned binary has those.
//!
//! # What the exit status proves, and why it needs no log parsing
//!
//! A process with NO handler for SIGTERM is **killed by the signal**: it has no exit code at all,
//! and `ExitStatus::signal()` is `Some(15)`. A process that handles it and returns from `main` has
//! `code() == Some(0)` and no signal. Those two are not a matter of degree — they are the before and
//! after of this entire change, readable from `waitpid` without reading a single line of output. The
//! stderr assertion below is the corroborating detail (which teardown outcome), not the proof.
//!
//! # …and the regression it guards in the same run
//!
//! Before the signal is sent, the daemon is started with **stdin at `/dev/null`** — the systemd
//! shape — and must still be alive and printing snapshot summaries. A non-tty EOF is NOT a stop:
//! treating it as one would exit the daemon at startup on every service box, every start. That trap
//! is the reason the stdio channel could never be the systemd stop path in the first place, so a
//! test of the fix that did not also hold the trap shut would be half a test.
//!
//! Unix only, and honestly so: there is no POSIX signal to send on Windows. That platform has its
//! own two triggers and its own twin of this file — `tests/windows_stop.rs`, which drives the same
//! binary in the same background shape through the stop file `vike_ops::stop::arm_stop_file`
//! watches. `kill(1)` is used here rather than `libc::kill`, which is `unsafe` and would need a
//! carve-out in a workspace that forbids it.

#![cfg(unix)]

use std::io::{BufRead, BufReader};
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How long to wait for a line of protocol, or for the process to die after the signal. Generous:
/// this is a shared, sometimes-loaded CI runner and every assertion here is about WHETHER something
/// happens, never how fast.
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

/// The project root one daemon is started in: an OWNED temp directory with the EMPTY `settings/`
/// store already inside it, removed when the returned guard drops.
///
/// ⚠ Returns the `TempDir` GUARD, and the caller must HOLD it for as long as the child runs —
/// [`Daemon`] binds it in a field declared AFTER `child`, so the reaper kills and reaps the daemon
/// before the directory it was started in is removed. That ordering is what the hand-rolled
/// `impl Drop for Daemon` this replaces got BACKWARDS: a type's own `drop` runs BEFORE its fields',
/// so the `remove_dir_all` fired while the child was still alive.
///
/// This used to be `env::temp_dir().join(format!("vike_tradehub_{tag}_{pid}_{nanos}"))`, cleaned
/// up by that `Drop` — so it leaked only on a hard kill, but it kept the OTHER defect in full.
/// MEASURED on the CI box, 2026-08-25 (the family total and the mechanism are in
/// `crates/vike-tradehub/src/config.rs`'s `own_script`): `/tmp` held 44,840 leaked directories of
/// this shape, and a REUSED pid colliding across the box's two test users (`the CI user` for CI,
/// `the operator` for the verification lanes) makes `create_dir_all` succeed on the OTHER user's
/// directory while the write into it fails `PermissionDenied`. A pid uniquifies WITHIN a run and
/// nothing across users over time; `tempfile` is unique by construction.
fn temp_dir(tag: &str) -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("vike_tradehub_{tag}_"))
        .tempdir()
        .expect("create the temp project root");
    std::fs::create_dir_all(dir.path().join("settings")).expect("create the temp settings dir");
    dir
}

/// A started daemon: the child, the project root it runs in, its stdout protocol lines, and
/// everything it has logged to stderr.
struct Daemon {
    child: Reaper,
    /// The temp project root, held so its `Drop` removes the tree at the end of the test.
    /// ⚠ Declared AFTER `child`: fields drop in declaration order, so the reaper stops the daemon
    /// before its working directory is taken away.
    _root: tempfile::TempDir,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Daemon {
    /// Start the shipped binary over the PAPER mount with stdin at `/dev/null` — the systemd shape.
    ///
    /// Every input is a temp directory: an empty `settings/` (so `VIKE_SETTINGS_DIR` resolves there
    /// rather than walking up into the developer's real credential store), its own log dir, and the
    /// file layer off. The removed-ceiling variables are cleared because a set one is a deliberate
    /// startup REFUSAL and the harness's own environment must not decide that.
    fn start(tag: &str) -> Self {
        let root = temp_dir(tag);
        let dir = root.path();
        let profile = dir.join("tradehub.toml");
        std::fs::write(
            &profile,
            // A minimal profile is just a token_id; the summary cadence is turned up so a couple of
            // protocol lines prove liveness in about a second instead of ten.
            "token_id = \"SIGTERM_STOP_TOKEN\"\n\
             [daemon]\n\
             summary_ms = 300\n\
             shutdown_deadline_ms = 5000\n",
        )
        .expect("write the daemon profile");

        let mut child = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
            .arg("--config")
            .arg(&profile)
            .current_dir(dir)
            .stdin(Stdio::null()) // the systemd shape: EOF at once, and NOT a stop
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("VIKE_SETTINGS_DIR", dir.join("settings"))
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
        // kernel buffer and the daemon then BLOCKS writing to it, which would look exactly like a
        // daemon ignoring a signal.
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

        Daemon { child: Reaper(child), _root: root, lines, stderr }
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

    /// Send a POSIX signal by name, through `kill(1)` — `libc::kill` is `unsafe`, and this
    /// workspace forbids `unsafe_code`.
    fn signal(&self, name: &str) {
        let status = Command::new("kill")
            .arg(format!("-{name}"))
            .arg(self.child.0.id().to_string())
            .status()
            .expect("run kill(1)");
        assert!(status.success(), "kill -{name} failed: {status:?}");
    }

    /// Wait for exit, or fail naming what the daemon logged on the way.
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            match self.child.0.try_wait().expect("try_wait") {
                Some(status) => return status,
                None if Instant::now() >= deadline => panic!(
                    "the daemon was still running {PATIENCE:?} after the signal — the graceful stop \
                     did not fire; stderr:\n{}",
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
// killed the daemon, deleting a running process's working directory (a no-op on Windows, a race on
// unix). Field order now reaps first and removes second.

/// ⚠ The whole change, end to end: a systemd-shaped daemon survives its stdin EOF, then a real
/// signal drives the ordinary teardown and the process exits **through `main`** rather than being
/// killed.
fn a_signal_stops_the_daemon_gracefully(signal: &str, tag: &str) {
    let mut daemon = Daemon::start(tag);

    let ready = daemon.next_line("ready");
    assert!(ready.contains("\"kind\":\"ready\""), "the daemon must announce itself: {ready:?}");
    // ...and it must announce itself PAPER. `Daemon::start` removes `VIKE_TRADEHUB_LIVE` and writes
    // no `flags.toml`, so the live gate is off by both layers — and the ready banner's `"mode"` is
    // the string `docs/ops/tradehub-the CI box.md` and `deploy/vike-tradehub-project.service` both call the
    // ONE authority on paper-vs-live. This is the only CI-gated BLACK-BOX check of that field over
    // the shipped binary (the LIVE-arm banner is asserted by `venue_feed_splice_smoke`'s driver,
    // whose cases are `#[ignore]`d live smokes), so it is what stands between a refactor of the
    // banner's computation and a paper daemon that stops saying so. Deliberately the exact string:
    // a `contains("PAPER")` would also pass on `LIVE (venue=…)` text that merely mentioned it.
    assert!(
        ready.contains("\"mode\":\"PAPER\""),
        "the live gate is off, so the ready banner must read exactly PAPER: {ready:?}"
    );

    // Liveness AFTER the stdin EOF that `Stdio::null()` delivered at startup. Two summary lines are
    // ~600 ms of a daemon that has read EOF and kept trading — the property that must not regress,
    // because treating that EOF as a stop would exit every service box at startup.
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

    daemon.signal(signal);
    let status = daemon.wait_for_exit();

    assert_eq!(
        status.signal(),
        None,
        "the daemon must HANDLE {signal} and exit through main — a process killed by the signal has \
         no exit code, runs no teardown, and abandons its resting book at the venue. stderr:\n{}",
        daemon.log()
    );
    assert_eq!(
        status.code(),
        Some(0),
        "…and exit 0, so a `Restart=on-failure` unit reads an operator stop as a stop. stderr:\n{}",
        daemon.log()
    );

    let log = daemon.log();
    // ⚠ This DEMANDS Graceful. It used to accept `"shut down gracefully" || "hard-capped"`, and
    // that `||` is precisely why nothing caught Finding C: a paper daemon with nothing outstanding
    // was spending its whole `shutdown_deadline_ms` on every stop and reporting
    // "hard-capped at the 10s deadline; 0 task(s) still in flight" — and this assertion passed,
    // because it only asked that the teardown said SOMETHING. A healthy stop of this fixture has
    // nothing to hard-cap on, so accepting the capped answer meant accepting the bug.
    assert!(
        log.contains("shut down gracefully"),
        "a healthy stop must be GRACEFUL, not deadline-capped — a hard cap here means the teardown \
         sat out its budget with nothing outstanding (the Finding C defect). stderr:\n{log}"
    );
    assert!(
        !log.contains("hard-capped"),
        "the teardown must not report a hard cap on a healthy stop; stderr:\n{log}"
    );
    assert!(!log.contains("ABORTED"), "the teardown orchestration must not panic; stderr:\n{log}");
}

#[test]
fn sigterm_runs_the_teardown_instead_of_killing_the_daemon() {
    a_signal_stops_the_daemon_gracefully("TERM", "sigterm");
}

/// SIGINT too, and for a reason beyond symmetry: Ctrl-C is what a human sends and SIGTERM is what a
/// supervisor sends, so a daemon that flushed on one and not the other would teach an operator a
/// rule that is wrong half the time.
#[test]
fn sigint_runs_the_teardown_too() {
    a_signal_stops_the_daemon_gracefully("INT", "sigint");
}
