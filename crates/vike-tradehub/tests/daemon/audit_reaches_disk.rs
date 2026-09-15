//! An accepted control command must leave a record ON DISK — at the file level the SHIPPED UNITS
//! actually set, which is `warn`.
//!
//! Every other audit test in this crate asserts the line's CONTENT through an in-memory subscriber
//! (`control_roundtrip.rs`'s `mod audit_capture`, `settings_write_audit.rs`'s twin). None of them
//! asserts it reached a FILE, and that is the exact gap that let the defect ship: `audit::record`
//! emits at `info`, `deploy/vike-tradehub.service` sets `Environment=VIKE_LOG_FILE_LEVEL=warn` to
//! bound the write rate, and the environment beats `LogConfig::file_level`. Measured on the live
//! the CI box box: a 23 MB daemon log held 53,160 ERROR lines, 1,928 WARN lines and ZERO INFO. A capture
//! subscriber cannot see any of that, because it never consults an `EnvFilter` at all.
//!
//! So this drives the REAL binary — `crates/vike-tradehub/tests/daemon/help_and_log_dir.rs`'s spawn
//! shape, `crates/vike-tradehub/tests/control_roundtrip.rs`'s handshake — with
//! `VIKE_LOG_FILE_LEVEL=warn` in its environment, and reads the rolling JSON file back off disk.
//! The fix it holds is `vike_tradehub::audit::FILE_PIN`, armed by `tradehub_cli::run`: remove
//! either half and this test fails.
//!
//! ⚠ **A GROUPED member, and the first draft was standalone for a reason that does not hold.** It
//! read "a process-global filter question in a binary full of process-global filter mutations" —
//! but the filter under test belongs to a CHILD PROCESS, composed by that process's own
//! `vike_log::init` from the environment this file hands it. No in-process subscriber any sibling
//! installs can reach it. By `crates/vike-backtest/CLAUDE.md`'s eligibility rule this file is plain
//! — no crate-level `#![cfg]`, no `#[ignore]`, no proptest sidecar, no process-global mutation (it
//! sets the child's environment through `Command`, never this process's) — so it groups like any
//! other member, and the exclusion list in `crates/vike-tradehub/tests/daemon.rs` stays a list of
//! real verdicts rather than gaining a decorative one.
//!
//! ⚠ The negative half is what makes it a proof rather than a coincidence. Asserting only that the
//! audit line is present would also pass on a daemon whose file level was `info` for some unrelated
//! reason — the very state the fix is NOT. So the test also asserts that the audit target is the
//! ONLY target with an `INFO` record in the file: everything else really is filtered at `warn`, and
//! the pin raised exactly one target and no more.

use std::io::{BufRead, BufReader};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_bridge_core::credentials::SECRETS_FILE;
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::WireCommand;

/// How long to wait for the daemon to bind, and for a written line to appear in the rolling file.
/// Generous: every assertion here is about WHETHER something happens, never how fast.
const PATIENCE: Duration = Duration::from_secs(30);

/// Obviously-fake HMAC secrets — any bytes work as long as both sides agree, and the `DUMMY-`
/// prefix makes it unmistakable in a diff or a process listing that no real credential is involved.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-audit-disk-test";
const CONTROL_KEY: &str = "DUMMY-control-key-for-the-audit-disk-test";

/// The coid and the rationale the one command carries. Both are distinctive strings, so finding
/// them in the file proves the WHOLE record survived the filter rather than some other `info` line
/// that happens to mention the daemon.
const COID: &str = "audit-reaches-disk-0001";
const REASON: &str = "flattening ahead of the audit-trail proof";

/// Kills the child if an assertion unwinds, so a failing test never leaves a trading daemon running
/// on the box. ⚠ Must be declared BEFORE the `TempDir` in [`Daemon`]: fields drop in declaration
/// order, and on Windows a running process's working directory cannot be removed at all.
struct Reaper(Child);

impl Drop for Reaper {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// A started daemon and the tree it runs in.
struct Daemon {
    _child: Reaper,
    /// Held so its `Drop` removes the tree — after the reaper above has reaped the child.
    _root: tempfile::TempDir,
    addr: SocketAddr,
    log_dir: PathBuf,
    stderr: Arc<Mutex<String>>,
}

impl Daemon {
    /// Everything the daemon reads is a temp directory or an explicit environment value, so this
    /// run cannot reach the developer's real settings, credential store or logs.
    ///
    /// The two gates are spelled as ENVIRONMENT rather than as `flags.toml` / `config.toml`
    /// deliberately: `VIKE_TRADEHUB_CONTROL=1` and `VIKE_TRADEHUB_ADDR` are the documented
    /// overrides for exactly those keys, and using them keeps the fixture to one file. What is NOT
    /// negotiable is `VIKE_LOG_FILE_LEVEL=warn` — that is the shipped units' own value and the
    /// entire subject of this test.
    fn start() -> Self {
        let root = tempfile::Builder::new()
            .prefix("vike_tradehub_audit_disk_")
            .tempdir()
            .expect("create the temp project root");
        let dir = root.path().to_path_buf();
        let settings = dir.join("settings");
        // The directory exists because the CREDENTIAL STORE is written into it on the next line —
        // that is all this call is load-bearing for. ⚠ It is deliberately NOT the daemon's state
        // root: `$VIKE_STATE_ROOT` is set below and `tradehub_cli`'s `state_dir` returns that
        // override outright, so the state tree, the log home this test reads and the HALT sentinel
        // all hang off `<dir>/state` and `settings/state` is never consulted. The `state`
        // component is kept only so the tree looks like a real project on inspection.
        std::fs::create_dir_all(settings.join("state")).expect("create the temp state dir");
        // The credential store IS the gate for the node keys — the daemon reads them from here, not
        // from its environment (`start_observe_server`'s `workspace_credentials()`).
        std::fs::write(
            settings.join(SECRETS_FILE),
            format!(
                "{}={OBSERVE_KEY}\n{}={CONTROL_KEY}\n",
                auth::OBSERVE_KEY_ENV,
                auth::CONTROL_KEY_ENV
            ),
        )
        .expect("write the throwaway credential store");
        let profile = dir.join("tradehub.toml");
        std::fs::write(
            &profile,
            // A minimal PAPER profile: a token_id is the whole requirement. The summary cadence is
            // turned up only so the daemon is visibly alive without a long wait.
            "token_id = \"AUDIT_DISK_TOKEN\"\n\
             [daemon]\n\
             summary_ms = 300\n\
             shutdown_deadline_ms = 5000\n",
        )
        .expect("write the daemon profile");

        // Claim an ephemeral port and hand the number to the daemon. The listener is dropped before
        // the spawn, so the daemon binds it — a small race the alternative (parsing the port back
        // out of the daemon's own log) would trade for a dependency on a log line's wording.
        let addr: SocketAddr = {
            let probe = TcpListener::bind("127.0.0.1:0").expect("claim an ephemeral loopback port");
            probe.local_addr().expect("resolve the assigned port")
        };
        let state_root = dir.join("state");

        let mut child = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"))
            .arg("--config")
            .arg(&profile)
            .current_dir(&dir)
            // The background-host shape: EOF at once on stdin, and that is NOT a stop.
            .stdin(Stdio::null())
            // Stdout is this daemon's PROTOCOL (a periodic JSON summary line). Discarded rather
            // than piped: nothing here reads it, and an undrained pipe would eventually fill and
            // BLOCK the daemon, which would look exactly like a daemon that never audited.
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env("VIKE_SETTINGS_DIR", &settings)
            .env("VIKE_STATE_ROOT", &state_root)
            .env("VIKE_TRADEHUB_ADDR", addr.to_string())
            .env("VIKE_TRADEHUB_CONTROL", "1")
            // ⚠ THE SUBJECT. This is `deploy/vike-tradehub.service`'s own value.
            .env("VIKE_LOG_FILE_LEVEL", "warn")
            // `$VIKE_LOG_DIR` outranks the state root, so it must be absent for the log to land
            // where this test reads it.
            .env_remove("VIKE_LOG_DIR")
            // A set removed-ceiling variable is a deliberate startup REFUSAL, and the harness's own
            // environment must not decide that for the daemon.
            .env_remove("VIKE_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
            .env_remove("VIKE_TRADEHUB_LIVE")
            .env_remove("VIKE_RECONCILE")
            .env_remove("RUST_LOG")
            .env_remove("VIKE_LOG")
            .spawn()
            .expect("spawn vike-tradehub");

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

        Daemon {
            _child: Reaper(child),
            _root: root,
            addr,
            log_dir: state_root.join("logs"),
            stderr,
        }
    }

    /// Everything the daemon has written to stderr so far — the diagnostic every failure below
    /// carries, because a daemon that refused to start is the likeliest cause of any of them.
    fn log(&self) -> String {
        self.stderr.lock().expect("stderr buffer").clone()
    }

    /// Connect once the daemon's control server is listening, or fail naming what it logged.
    ///
    /// Polls `connect` rather than watching for a log line: the socket is the thing the next step
    /// actually needs, and a test that waited on a message would keep passing after the daemon
    /// stopped binding and start failing after a reworded log line.
    fn connect_when_listening(&self) -> TcpStream {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Ok(s) = TcpStream::connect(self.addr) {
                return s;
            }
            assert!(
                Instant::now() < deadline,
                "the daemon never accepted a connection on {} within {PATIENCE:?}; stderr:\n{}",
                self.addr,
                self.log()
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// Every rolling log file the daemon wrote, concatenated — and, separately, every read that
/// FAILED.
///
/// ⚠ The two must not be conflated, which is what an `unwrap_or_default()` per file would do. A
/// read can fail for reasons that have nothing to do with the filter under test — a partially
/// flushed multi-byte sequence at the tail comes back `InvalidData`, and a handle or permission
/// fault comes back as itself — and every one of them yields an empty string for that file. The
/// caller's failure message asserts the M2 defect by name, so a swallowed read error would send
/// the next engineer to bisect a logging-filter regression that did not happen. Errors are
/// returned beside the body and printed instead.
///
/// An ABSENT directory is not an error: the daemon has simply not written yet, and the caller polls.
fn log_body(dir: &Path) -> (String, Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (String::new(), Vec::new());
    };
    let mut body = String::new();
    let mut errors = Vec::new();
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("vike-tradehub.") {
            continue;
        }
        match std::fs::read_to_string(entry.path()) {
            Ok(text) => body.push_str(&text),
            Err(e) => errors.push(format!("{}: {e}", entry.path().display())),
        }
    }
    (body, errors)
}

/// Complete the `Control` handshake on an already-connected stream.
fn authenticate_control(stream: &mut TcpStream) {
    write_frame(stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(CONTROL_KEY.as_bytes(), &nonce, NODE_PROTO_VERSION, Scope::Control);
    write_frame(stream, &Request::Auth { scope: Scope::Control, mac }).expect("auth");
    match read_frame::<_, Response>(stream).expect("authok") {
        Response::AuthOk { scope: Scope::Control } => {}
        other => panic!("expected AuthOk(Control), got {other:?}"),
    }
}

/// **The M2 gate.** One accepted control command against the real daemon, at the real file level.
///
/// A `Cancel` deliberately, rather than a submit: it lowers unconditionally
/// (`vike_tradehub::server`'s `lower_command` takes any coid), needs no mount, no price and no
/// venue, and is audited by exactly the same `audit::record` call every other accepted verb
/// reaches. The subject here is the FILE LAYER, so the command should contribute as few reasons to
/// fail as possible.
#[test]
fn an_accepted_command_is_on_disk_even_at_file_level_warn() {
    let daemon = Daemon::start();
    let mut ctl = daemon.connect_when_listening();
    authenticate_control(&mut ctl);

    write_frame(
        &mut ctl,
        &Request::Command {
            cmd: WireCommand::Cancel(COID.to_string()),
            reason: Some(REASON.to_string()),
        },
    )
    .expect("send the control command");
    match read_frame::<_, Response>(&mut ctl).expect("response") {
        Response::Ack { coid } => assert_eq!(coid, COID, "the command was ACCEPTED, coid echoed"),
        other => panic!(
            "the command must be accepted before its record can be asserted, got {other:?}; \
             stderr:\n{}",
            daemon.log()
        ),
    }

    // The non-blocking appender writes on its own thread, so the record lands a moment after the
    // ack. Polled rather than slept: the file is the observable, and waiting for it is honest where
    // a fixed sleep would be a guess that gets tuned until it passes.
    //
    // ⚠ What is waited FOR is a COMPLETE, PARSEABLE record on the pinned target, not a substring.
    // A rolling file read while the daemon is still running can end mid-line, and a torn tail that
    // happens to contain the message would break this loop with a body the negative half below
    // cannot parse — reported as "the pin raised nothing", which is a lie about the subject.
    let deadline = Instant::now() + PATIENCE;
    let (body, read_errors) = loop {
        let (body, read_errors) = log_body(&daemon.log_dir);
        if json_records(&body).iter().any(|r| {
            level_of(r) == Some("INFO") && target_of(r) == vike_tradehub::audit::FILE_PIN.0
        }) {
            break (body, read_errors);
        }
        if Instant::now() >= deadline {
            panic!(
                "an accepted command left NO parseable record on disk at file level warn — this is \
                 the M2 defect UNLESS a read failed, which is why the failed reads are listed \
                 first. {} held {} bytes; reads that failed: {read_errors:?}\n{body:.2000}\n\
                 --- daemon stderr:\n{}",
                daemon.log_dir.display(),
                body.len(),
                daemon.log()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    // A read that failed is not evidence about the filter either way, but it silently shrinks the
    // body every assertion below reads — so it is reported rather than swallowed.
    assert!(read_errors.is_empty(), "a log file could not be read: {read_errors:?}");

    // …and the WHOLE record survived, not merely SOME record on the pinned target.
    assert!(
        body.contains("vike-tradehub control: command accepted"),
        "the pinned target reached disk but not with the accepted-command line:\n{body:.2000}"
    );
    assert!(body.contains(COID), "the audit record must carry the coid it targeted:\n{body:.2000}");
    assert!(
        body.contains(REASON),
        "…and the operator's rationale, which is the half an incident review reads:\n{body:.2000}"
    );

    // THE NEGATIVE HALF. `warn` really was in effect, and the pin raised EXACTLY one target: the
    // only `INFO` records in the file are the audit trail's own. Without this, a daemon running at
    // `info` for an unrelated reason would pass the assertions above while proving nothing. The
    // list cannot be EMPTY here — the loop above already found an `INFO` record on the pinned
    // target in this same body — so the only failure this can report is an EXTRA target.
    let mut info_targets: Vec<String> = Vec::new();
    for record in json_records(&body) {
        if level_of(&record) == Some("INFO") {
            let target = target_of(&record).to_string();
            if !info_targets.contains(&target) {
                info_targets.push(target);
            }
        }
    }
    assert_eq!(
        info_targets,
        vec![vike_tradehub::audit::FILE_PIN.0.to_string()],
        "at VIKE_LOG_FILE_LEVEL=warn the ONLY info-level target in the file must be the pinned \
         audit target — a second one means the global level was not really `warn`, and the pin \
         proved nothing. File:\n{body:.2000}"
    );
}

/// Every COMPLETE JSON record in a log body, in order.
///
/// A partially-written trailing line is possible while the daemon is still running; it is not
/// evidence about the filter either way, so it is skipped rather than failed on.
fn json_records(body: &str) -> Vec<serde_json::Value> {
    body.lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect()
}

/// The `tracing` JSON layer's level field ("INFO", "WARN", …), or `None` when the record has none.
fn level_of(record: &serde_json::Value) -> Option<&str> {
    record.get("level").and_then(|l| l.as_str())
}

/// The record's target, or a placeholder — a record with no target is still a record whose presence
/// at `INFO` would falsify the negative half, so it must not be silently dropped.
fn target_of(record: &serde_json::Value) -> &str {
    record.get("target").and_then(|t| t.as_str()).unwrap_or("<no target>")
}
