//! **A credential store that EXISTS and will not open must not read as a box that has none** —
//! proved against the REAL binary, the REAL boot sequence and the REAL credential loader.
//!
//! # The failure this is the regression test for, measured
//!
//! The store is a SQLite file. A writer killed mid-transaction leaves a HOT ROLLBACK JOURNAL
//! beside it, and the read-only open a daemon performs is then refused whole
//! (`SQLITE_READONLY_ROLLBACK`) at the first statement. `vike_bridge_core::credentials`'s
//! infallible loader logs one line and returns an EMPTY map — and an empty credential map is not
//! an error downstream, it IS the live gate. So every venue dropped to paper, **nothing failed**,
//! `Restart=on-failure` never fired, `OnFailure=` never paged, and the line an operator greps
//! (`"kind":"ready"`) came back `LIVE (venue=none)` — which
//! `deploy/vike-tradehub.service`'s own header documents as a legitimate answer: gate on,
//! nothing armed. A live daemon that had lost its keys and a correctly-unarmed box printed the
//! same string.
//!
//! # Why the store here is CORRUPT rather than hot-journalled
//!
//! What this file is about is the DAEMON's disposition for `StoreHealth::Unreadable`, and every
//! cause reaches that one arm. Building a hot journal is a measurement of the STORE layer, it is
//! platform-specific, and it already has a home — `crates/vike-secrets/tests/` gained one on the
//! branch that measured it, which plants a real journal and pins what the opener does with it. A
//! file of bytes that is not a SQLite database reaches the identical `Unreadable` arm through the
//! identical production path, on every platform, with no timing and no signals — so that is what is
//! planted here, and nothing about the CAUSE is asserted.
//!
//! # Scope: the gate is OFF here, and the measured failure was a LIVE daemon
//!
//! A live mount DIALS real venue feeds, which is why `tests/venue_feed_splice_smoke.rs`'s live
//! cases are `#[ignore]`d, so it cannot be a PR-gated test. The live/paper boolean is an
//! INDEPENDENT input to the renderer — the exact `CREDENTIAL STORE UNREADABLE — LIVE (venue=none)`
//! string is pinned by `tradehub_cli.rs`'s
//! `an_unreadable_store_is_not_the_same_banner_as_a_correctly_unarmed_box` — and what only a real
//! process can prove is that the verdict reaches the renderer AT ALL. That is what runs here, and
//! `Daemon::start` argues it again where it is done.
//!
//! # Why a spawned binary rather than a unit test
//!
//! The renderer has unit tests (`ready_mode_line`'s own, in `tradehub_cli.rs`). They cannot reach
//! the question this file asks, which is whether the daemon's `main` still THROWS THE VERDICT
//! AWAY: the defect was never in the rendering, it was that `workspace_credentials` called the
//! infallible loader and the `StoreHealth` was computed and discarded at the one root that signs
//! orders. Only a real process running the real boot proves that it no longer is.
//!
//! The harness is `tests/sigterm_stop.rs`'s, minus the signal — that file's traps are its own
//! (`_root` declared after `_child` so the reaper stops the daemon before its working directory is
//! taken away; both pipes drained on their own threads because an undrained pipe blocks the
//! daemon). No signal is sent here, so unlike its sibling this one is not unix-only.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// How long to wait for the ready banner. Generous: this is a shared, sometimes-loaded CI runner,
/// and every assertion here is about WHAT is printed, never how fast.
const PATIENCE: Duration = Duration::from_secs(60);

/// Kills the child if an assertion unwinds, so a failing test never leaves a trading daemon running
/// on the box.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// What to put in `<root>/settings/` before the daemon starts.
#[derive(Clone, Copy)]
enum Store {
    /// Nothing at all — the ordinary UNCONFIGURED box, and the state this change must leave
    /// byte-identical.
    Absent,
    /// `settings/db/vike.db` exists and is not a database. `vike_secrets`' backend probe decides
    /// which store answers from ONE `is_file` on that path, so this file IS the store: the engine
    /// refuses it, and the refusal arrives at the daemon as `StoreHealth::Unreadable`.
    PresentAndUnreadable,
}

struct Daemon {
    /// Held only for its `Drop`, which kills and reaps — nothing here reads the child, unlike
    /// `sigterm_stop.rs`, which signals it.
    _child: Reaper,
    /// ⚠ Declared AFTER `_child`: fields drop in declaration order, so the reaper stops the daemon
    /// before its working directory is taken away.
    _root: tempfile::TempDir,
    lines: Receiver<String>,
    stderr: Arc<Mutex<String>>,
}

impl Daemon {
    /// Start the shipped binary in its own temp project root, with the live gate as asked and the
    /// planted store in place.
    ///
    /// ⚠ `VIKE_SETTINGS_DIR` is set so the project walk resolves into the temp tree rather than
    /// climbing into the developer's or the runner's REAL credential store. Every removed-ceiling
    /// variable is cleared because a set one is a deliberate startup refusal and the harness's own
    /// environment must not decide that.
    fn start(tag: &str, store: Store) -> Self {
        let root = tempfile::Builder::new()
            .prefix(&format!("vike_tradehub_{tag}_"))
            .tempdir()
            .expect("create the temp project root");
        let dir = root.path();
        let settings = dir.join("settings");
        std::fs::create_dir_all(&settings).expect("create the temp settings dir");
        if matches!(store, Store::PresentAndUnreadable) {
            let db_dir = settings.join("db");
            std::fs::create_dir_all(&db_dir).expect("create the temp db dir");
            // Not a SQLite file, and deliberately not EMPTY: a zero-length file IS a valid empty
            // SQLite database (`crates/vike-secrets/src/db.rs` says so at length), so an empty one
            // would open, read no rows, and test nothing.
            std::fs::write(db_dir.join("vike.db"), b"this is not a database, it is bytes")
                .expect("plant the unreadable store");
        }

        let profile = dir.join("tradehub.toml");
        std::fs::write(
            &profile,
            "token_id = \"STORE_HEALTH_TOKEN\"\n\
             [daemon]\n\
             summary_ms = 300\n\
             shutdown_deadline_ms = 5000\n",
        )
        .expect("write the daemon profile");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"));
        cmd.arg("--config")
            .arg(&profile)
            .current_dir(dir)
            .stdin(Stdio::null()) // the systemd shape: EOF at once, and NOT a stop
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("VIKE_SETTINGS_DIR", &settings)
            .env("VIKE_STATE_ROOT", dir.join("state"))
            .env("VIKE_LOG_DIR", dir.join("logs"))
            .env("VIKE_LOG_FILE_LEVEL", "off")
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_ADDR")
            .env_remove("VIKE_RECONCILE")
            .env_remove("RUST_LOG")
            .env_remove("VIKE_LOG")
            // ⚠ **THE GATE IS OFF HERE, AND THAT IS A SCOPE LIMIT WORTH STATING RATHER THAN
            // HIDING.** The measured the CI box failure was the LIVE arm (`LIVE (venue=none)`), and a
            // live daemon cannot be stood up in a PR-gated test: the profile would need a
            // live-wireable venue, and `wire_venue_feeds` then DIALS that venue's real market-data
            // socket — which is exactly why `tests/venue_feed_splice_smoke.rs`'s live cases are
            // `#[ignore]`d. What this file proves is the half a unit test cannot: that the daemon's
            // `main` no longer THROWS THE VERDICT AWAY — the same `credential_store_health()`, the
            // same `error!`, the same `ready_mode_line` call, over the real store on disk. The
            // `live` boolean is an independent input to that renderer, and the exact LIVE string is
            // pinned by `tradehub_cli.rs`'s
            // `an_unreadable_store_is_not_the_same_banner_as_a_correctly_unarmed_box`.
            .env_remove("VIKE_TRADEHUB_LIVE");
        let mut child = cmd.spawn().expect("spawn vike-tradehub");

        // Both pipes drained on their own threads: an undrained pipe fills its kernel buffer and
        // the daemon then BLOCKS writing to it.
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
                let mut buf = sink.lock().expect("stderr buffer");
                buf.push_str(&line);
                buf.push('\n');
            }
        });

        Daemon { _child: Reaper(child), _root: root, lines, stderr }
    }

    /// The ready banner — the FIRST protocol line, printed synchronously before the stdio thread
    /// starts. A dead daemon closes the pipe, so this never hangs past [`PATIENCE`].
    fn ready(&self) -> String {
        self.lines.recv_timeout(PATIENCE).unwrap_or_else(|e| {
            panic!("no ready banner within {PATIENCE:?} ({e}); stderr:\n{}", self.log())
        })
    }

    /// The next protocol line on stdout after the banner — a `summary`. Read purely to prove TIME
    /// has passed inside the daemon, so a NEGATIVE assertion about the log is not merely winning a
    /// race with the stderr drain thread.
    fn next_line(&self) -> String {
        self.lines.recv_timeout(PATIENCE).unwrap_or_else(|e| {
            panic!("no further protocol line within {PATIENCE:?} ({e}); stderr:\n{}", self.log())
        })
    }

    fn log(&self) -> String {
        self.stderr.lock().expect("stderr buffer").clone()
    }

    /// Wait for `needle` to appear in the daemon's stderr.
    ///
    /// ⚠ A poll rather than one read, and not for flakiness' sake: stderr is drained by its own
    /// thread, so a line the daemon has already WRITTEN may not yet be in the buffer when the
    /// stdout line that followed it arrives. Asserting on a single snapshot would be racing the
    /// drain, and the failure would look exactly like a missing log line.
    fn expect_log(&self, needle: &str, why: &str) {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            if self.log().contains(needle) {
                return;
            }
            if std::time::Instant::now() >= deadline {
                panic!("{why}\nlooked for {needle:?} in the daemon's log:\n{}", self.log());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// The banner's `"mode"` field — the string `docs/ops/tradehub-the CI box.md` and
/// `deploy/vike-tradehub.service` both call the ONE authority on paper-vs-live.
fn mode_of(ready: &str) -> String {
    let v: serde_json::Value =
        serde_json::from_str(ready).unwrap_or_else(|e| panic!("banner is not JSON ({e}): {ready}"));
    assert_eq!(v["kind"], "ready", "not the ready banner: {ready}");
    v["mode"].as_str().unwrap_or_else(|| panic!("no string `mode`: {ready}")).to_string()
}

/// ⚠ **THE REQUIREMENT THAT CONSTRAINS THE FIX: an ABSENT store changes NOTHING.**
///
/// No store on the box is the ordinary unconfigured state, not a fault. The empty credential map is
/// a real measurement, the live gate (no creds ⇒ every venue paper) is working as designed, and
/// this daemon must print exactly the banner it printed before any of this existed — and must not
/// have acquired a new complaint on the way.
#[test]
fn an_absent_store_is_byte_identical_to_before() {
    let daemon = Daemon::start("absent", Store::Absent);
    let ready = daemon.ready();
    assert_eq!(
        mode_of(&ready),
        "PAPER",
        "a box with no credential store must print exactly PAPER: {ready}"
    );

    // ⚠ Two further protocol lines before the NEGATIVE assertion below. The error arm — if it
    // fired — would be written BEFORE the banner, but stderr is drained by another thread, so
    // reading the buffer the instant the banner arrives would be racing it and a green would mean
    // nothing. Two summaries at `summary_ms = 300` is the daemon telling us it has moved on.
    let _ = daemon.next_line();
    let _ = daemon.next_line();

    let log = daemon.log();
    assert!(
        !log.contains("CREDENTIAL STORE IS PRESENT AND UNREADABLE"),
        "an absent store is not a fault and must raise no finding:\n{log}"
    );
    assert!(
        !log.contains("could not be opened"),
        "an absent store must not reach the unreadable-store error arm:\n{log}"
    );
}

/// ⚠ **THE BUG, end to end, against the shipped binary.**
///
/// A store that is PRESENT and will not open. Before this change the daemon's banner was whatever
/// a box with NO store prints — `PAPER` here, `LIVE (venue=none)` on the box where this was
/// measured — because the `StoreHealth::Unreadable` value that exists precisely to tell those
/// apart was computed by the loader and discarded by `workspace_credentials`.
///
/// Four assertions, and each one fails a different way of getting this wrong:
///
/// * the banner is NOT the string a correctly-configured box prints — the indistinguishability
///   itself, which is the whole defect;
/// * it still ANSWERS paper-vs-live — a fix that replaced the answer rather than adding to it
///   would break every runbook and every operator grep;
/// * the operator's log names the REPAIR and says the daemon cannot perform it — because the one
///   thing worse than an invisible fault is a visible one nobody can act on;
/// * ...and the daemon is STILL RUNNING to print any of it, which is the disposition this change
///   chose over refusing to start.
#[test]
fn a_present_but_unreadable_store_is_unmistakable_and_names_its_repair() {
    let daemon = Daemon::start("unreadable", Store::PresentAndUnreadable);
    let ready = daemon.ready();
    let mode = mode_of(&ready);

    assert_ne!(
        mode, "PAPER",
        "a daemon whose credential store will not open printed the banner of a box that simply \
         has no store — the one line an operator greps cannot say why every venue is on paper: \
         {ready}"
    );
    assert_eq!(
        mode, "CREDENTIAL STORE UNREADABLE — PAPER",
        "the banner must LEAD with the fault and still END with the paper-vs-live answer, or \
         either the finding is missable or every existing grep and runbook stops working: {ready}"
    );

    daemon.expect_log(
        "CREDENTIAL STORE IS PRESENT AND UNREADABLE",
        "the fault must be stated in the operator's log, not only in the banner",
    );
    // The REPAIR, and who must perform it. `vike-cli secrets` is named because opening the store
    // READ-WRITE is what replays a rollback journal — no verb, no flag, no repair tool — and it
    // must be run from an operator shell, because the daemon's own settings directory is read-only
    // in its own mount namespace.
    daemon.expect_log("vike-cli secrets", "the message must name the command that repairs it");
    daemon.expect_log(
        "OPERATOR SHELL",
        "...and where it must be run from, since the daemon's namespace is the one place it does \
         not work",
    );
    daemon.expect_log(
        "CANNOT REPAIR THIS ITSELF",
        "...and that a restart achieves nothing, which is why this daemon does not refuse to start",
    );
    daemon.expect_log(
        "EVERY VENUE IS ON PAPER",
        "...and what the daemon is actually doing meanwhile",
    );

    // ⚠ THE DISPOSITION ITSELF: the daemon is still alive. Refusing to start was the alternative,
    // and it would have been a RESTART LOOP — nothing inside the unit's mount namespace can replay
    // the journal, so every restart meets the identical state. A daemon that stays up keeps its
    // control surface reachable and its teardown intact; a crash-looping one answers nothing.
    let next = daemon.next_line();
    assert!(
        next.contains("\"kind\":\"summary\""),
        "the daemon must keep running and reporting, not refuse to start: {next}"
    );
}
