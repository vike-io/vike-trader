//! **A credential store that EXISTS and will not open must not read as a box that has none** —
//! proved against the REAL binary, the REAL boot sequence and the REAL credential loader.
//!
//! ⚠ **Decision 0086 ("settings live only in the database") NARROWED what this file can
//! prove, and this note says so rather than letting the narrowing hide in a rewritten assertion.**
//! Before 0086 the daemon's MOUNT came from a `--config` file, entirely independent of whether the
//! settings database (credentials, profile rows — one file since 0054) could be opened; an
//! unreadable database therefore only ever cost the CREDENTIAL half, and the whole point measured
//! below is that this daemon stays alive on that fault rather than restart-looping. 0086 retired the
//! file rung: the daemon's mount now comes from an ACTIVE PROFILE ROW in that same database, so a
//! database that will not open now ALSO means the daemon cannot learn what to mount, and it refuses
//! to start (`crate::tradehub_cli`'s `run`, the arm following `profile_store_unreadable`). The
//! CREDENTIAL claim below is UNCHANGED and still proven here — an unreadable store still yields an
//! EMPTY credential map, never a refusal, and the live gate still drops every venue to paper — it is
//! reached now by first seeding a profile row into a store that opens fine and then testing the
//! credential-loading arm in isolation, rather than by planting one corrupt file that used to stand
//! in for both. `a_present_but_unreadable_store_now_refuses_naming_both_repairs` is the test that
//! proves the boundary moved, on the real binary, rather than asserting it in prose here.
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
//! (`_root` declared after `child` so the reaper stops the daemon before its working directory is
//! taken away; both pipes drained on their own threads because an undrained pipe blocks the
//! daemon). No signal is sent here, so unlike its sibling this one is not unix-only.

#[path = "support/daemon_profile_seed.rs"]
mod daemon_profile_seed;

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
    /// A store that OPENS FINE and carries an active daemon-profile row (0086 requires one to run
    /// at all — see this file's module doc) but NO credential rows. The ordinary "this box has no
    /// credentials configured" state on the CREDENTIAL half, which is what this variant tests —
    /// distinct from [`Self::PresentAndUnreadable`], where the STORE ITSELF cannot be opened.
    NoCredentials,
    /// `settings/db/vike.db` exists and is not a database. `vike_secrets`' backend probe decides
    /// which store answers from ONE `is_file` on that path, so this file IS the store: the engine
    /// refuses it, and the refusal arrives at the daemon as `StoreHealth::Unreadable` — and, since
    /// 0086, ALSO as a daemon-profile refusal, because the same open failure takes the mount
    /// configuration with it. There is no way to seed a profile row here: a row would have to live
    /// inside the very file this variant makes unopenable.
    PresentAndUnreadable,
}

struct Daemon {
    /// Held for its `Drop`, which kills and reaps, and — since 0086 can make this daemon refuse to
    /// start rather than stay alive — read directly by [`Daemon::wait_for_exit`] too.
    child: Reaper,
    /// ⚠ Declared AFTER `child`: fields drop in declaration order, so the reaper stops the daemon
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
        match store {
            // ⚠ 0086: the daemon reads no profile FILE any more — seed the ACTIVE daemon-profile ROW
            // this box needs to run at all into a store that otherwise opens fine and carries no
            // credentials.
            Store::NoCredentials => {
                daemon_profile_seed::seed_active_daemon_profile(
                    &settings,
                    "store-health",
                    "token_id = \"STORE_HEALTH_TOKEN\"\n\
                     asset_class = \"PredictionMarket\"\n\
                     [daemon]\n\
                     summary_ms = 300\n\
                     shutdown_deadline_ms = 5000\n",
                );
            }
            Store::PresentAndUnreadable => {
                let db_dir = settings.join("db");
                std::fs::create_dir_all(&db_dir).expect("create the temp db dir");
                // Not a SQLite file, and deliberately not EMPTY: a zero-length file IS a valid empty
                // SQLite database (`crates/vike-secrets/src/db.rs` says so at length), so an empty
                // one would open, read no rows, and test nothing. No profile row can be seeded here
                // — see this variant's own doc.
                std::fs::write(db_dir.join("vike.db"), b"this is not a database, it is bytes")
                    .expect("plant the unreadable store");
            }
        }

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"));
        cmd.current_dir(dir)
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

        Daemon { child: Reaper(child), _root: root, lines, stderr }
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

    /// Wait for the process to exit, or fail naming what it logged on the way — the shape a REFUSAL
    /// case needs (0086 can now make this daemon exit before ever printing a ready banner) rather
    /// than the still-running shape [`Self::ready`]/[`Self::next_line`] serve.
    fn wait_for_exit(&mut self) -> std::process::ExitStatus {
        let deadline = std::time::Instant::now() + PATIENCE;
        loop {
            match self.child.0.try_wait().expect("try_wait") {
                Some(status) => return status,
                None if std::time::Instant::now() >= deadline => panic!(
                    "the daemon was still running {PATIENCE:?} after start — expected it to have \
                     refused already; stderr:\n{}",
                    self.log()
                ),
                None => std::thread::sleep(Duration::from_millis(50)),
            }
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

/// ⚠ **THE REQUIREMENT THAT CONSTRAINS THE FIX: NO CREDENTIALS changes NOTHING.**
///
/// A box with an active daemon-profile row (0086 requires one to run at all — see this file's
/// module doc) and no credential rows is the ordinary unconfigured-CREDENTIALS state, not a fault.
/// The empty credential map is a real measurement, the live gate (no creds ⇒ every venue paper) is
/// working as designed, and this daemon must print exactly the banner a box with no credential
/// store printed before 0086 — and must not have acquired a new complaint on the way.
///
/// ⚠ This used to start the daemon with NO settings store at all (a variant this file no longer has,
/// named `Absent`) and call that state BYTE-IDENTICAL to before. It no longer can be: 0086 retired
/// the `--config` file rung, so a box with no settings store also has no daemon-profile row and this
/// daemon now REFUSES to start (see `a_present_but_unreadable_store_now_refuses_naming_both_repairs`
/// below, which proves the refusal on a store that cannot even be opened — the closest reachable
/// analogue, since a genuinely absent store cannot be told apart from that any more) — a genuine
/// narrowing this file's own module doc argues rather than hides. What survives unchanged, and what
/// this test now isolates by seeding a profile row into a store that opens fine, is the CREDENTIAL
/// claim alone.
#[test]
fn a_store_with_a_profile_row_and_no_credentials_reports_no_fault() {
    let daemon = Daemon::start("nocreds", Store::NoCredentials);
    let ready = daemon.ready();
    assert_eq!(
        mode_of(&ready),
        "PAPER",
        "a box with an active profile row and no credentials must print exactly PAPER: {ready}"
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
        "a store with no credentials is not a fault and must raise no finding:\n{log}"
    );
    assert!(
        !log.contains("could not be opened"),
        "a store with no credentials must not reach the unreadable-store error arm:\n{log}"
    );
}

/// ⚠ **THE BUG this file was written for, and the DISPOSITION 0086 then narrowed — both proved
/// here, against the shipped binary.**
///
/// A store that is PRESENT and will not open. Before 0086 the daemon's banner was whatever a box
/// with NO store prints — `PAPER` here, `LIVE (venue=none)` on the box where this was measured —
/// because the `StoreHealth::Unreadable` value that exists precisely to tell those apart was
/// computed by the loader and discarded by `workspace_credentials`, and the fix made the daemon
/// STAY ALIVE and announce the fault rather than refuse to start, because refusing would have been a
/// RESTART LOOP: nothing inside the unit's mount namespace can replay the journal, so every restart
/// met the identical state.
///
/// ⚠ **0086 removed the "stay alive" half of that fix, and this test now proves the removal rather
/// than the original claim.** The same unopenable file holds this daemon's own mount configuration
/// now, so there is no profile to run ON PAPER WITH — the daemon refuses to start, still names the
/// fault and the repair (the restart-loop argument is unchanged; refusing here costs nothing further
/// because nothing was going to run either way), and ALSO says that `bootstrap-daemon` — the cure
/// for an ordinary missing row — cannot fix an unopenable store, so an operator does not try the
/// wrong tool first.
#[test]
fn a_present_but_unreadable_store_now_refuses_naming_both_repairs() {
    let mut daemon = Daemon::start("unreadable", Store::PresentAndUnreadable);

    let status = daemon.wait_for_exit();
    assert!(
        !status.success(),
        "an unopenable settings store must refuse to start rather than run with an unknown mount \
         (or none at all); stderr:\n{}",
        daemon.log()
    );

    daemon.expect_log(
        "could not be read",
        "the profile-read fault must be stated in the operator's log",
    );
    // The REPAIR, and who must perform it — unchanged in substance from the credential-only fault
    // this used to be the whole of. `vike-cli secrets` is named because opening the store READ-WRITE
    // is what replays a rollback journal — no verb, no flag, no repair tool — and it must be run from
    // an operator shell, because the daemon's own settings directory is read-only in its own mount
    // namespace.
    daemon.expect_log("vike-cli secrets", "the message must name the command that repairs it");
    daemon.expect_log(
        "OPERATOR SHELL",
        "...and where it must be run from, since the daemon's namespace is the one place it does \
         not work",
    );
    daemon.expect_log(
        "CANNOT REPAIR THIS ITSELF",
        "...and that a restart achieves nothing on its own",
    );
    // ...and the NEW half: naming the tool that does NOT fix this, so an operator does not spend a
    // restart on the wrong repair.
    daemon.expect_log(
        "bootstrap-daemon",
        "...and must name the writer that CANNOT repair an unopenable store, so an operator does \
         not try it first",
    );
}
