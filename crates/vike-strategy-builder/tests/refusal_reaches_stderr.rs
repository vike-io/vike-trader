//! A refused `Build` must reach the JOURNAL, not only the peer that asked — proven against the
//! real binary, over the real socket, by reading the real stderr.
//!
//! ⚠ **Why a spawned process and not a call to the formatter.** The defect this pins was not a
//! badly-worded message; it was a message that went NOWHERE an operator looks. MEASURED on a
//! deployed box: after a release replaced the binary and not its source tree,
//! `render::verify_source_version` refused every Build for two days, the refusal travelled back
//! over the wire to whichever Studio asked, and `journalctl -u vike-strategy-builder` showed the
//! two listening banners and nothing else — a daemon refusing every request read exactly like an
//! idle one. systemd's journal is the daemon's STDERR, so stderr of the real binary is the one
//! place this can be proven; a unit test of `build_refusal_line` would stay green with the call
//! site deleted. (That function's own content rules — no source, no forged second line — are
//! pinned beside it, in `crates/vike-strategy-builder/src/builder.rs`'s in-file tests.)
//!
//! ⚠ **No port is released and re-bound.** The daemon is started on port `0` and this test reads
//! the port it actually bound from its own listening banner, which prints the socket's real
//! address for exactly this reason (`builder::run`). The probe-drop-spawn shape is the ephemeral
//! port race other daemon tests in this tree have already paid for.
//!
//! Cheap and un-ignored: the version refusal fires BEFORE `cargo` is ever invoked, so nothing here
//! compiles a plugin.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use vike_node_proto::auth::NodeKeys;
use vike_strategy_builder::builder::{BUILDER_KEY_ENV, WORKSPACE_ROOT_ENV};
use vike_strategy_builder::client::{BuildRequestError, build_remote};
use vike_strategy_builder::render::SOURCE_VERSION_STAMP_FILE;

/// The daemon, killed however the test ends — a panicking assertion must not leave a listener
/// behind on a shared CI box.
struct Daemon(Child);

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Every stderr line the daemon prints, as it prints it.
fn stderr_lines(child: &mut Child) -> Receiver<String> {
    let stderr = child.stderr.take().expect("stderr was piped");
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// The first line satisfying `want` within `limit`, with everything seen on the way — so a failure
/// names what the daemon DID say rather than only that the wanted line never came.
fn wait_for(
    rx: &Receiver<String>,
    limit: Duration,
    want: impl Fn(&str) -> bool,
) -> Result<String, Vec<String>> {
    let deadline = Instant::now() + limit;
    let mut seen = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(seen);
        }
        match rx.recv_timeout(left) {
            Ok(line) if want(&line) => return Ok(line),
            Ok(line) => seen.push(line),
            Err(_) => return Err(seen),
        }
    }
}

#[test]
fn a_refused_build_is_said_on_the_daemons_stderr() {
    let root = tempfile::tempdir().expect("tempdir");
    let out = tempfile::tempdir().expect("tempdir");
    // A stamp naming a commit this binary is not: the exact shape a release leaves behind when it
    // moves the binary and not the tree.
    let stale_stamp = "0000000";
    assert_ne!(
        stale_stamp,
        vike_buildinfo::GIT_SHA,
        "the planted stamp collided with the binary's own commit — this test would prove nothing"
    );
    std::fs::write(root.path().join(SOURCE_VERSION_STAMP_FILE), stale_stamp).expect("stamp");

    let key = "refusal-test-key-not-a-real-credential";
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-strategy-builder"))
        .env(BUILDER_KEY_ENV, key)
        .env(WORKSPACE_ROOT_ENV, root.path())
        .env("VIKE_STRATEGY_BUILDER_PORT", "0")
        .env("VIKE_STRATEGY_BUILDER_OUT_DIR", out.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the builder binary");
    let rx = stderr_lines(&mut child);
    let _daemon = Daemon(child);

    let banner = wait_for(&rx, Duration::from_secs(60), |l| l.contains("listening on"))
        .unwrap_or_else(|seen| panic!("the daemon never printed its listening banner: {seen:#?}"));
    let addr = banner
        .split("listening on ")
        .nth(1)
        .and_then(|rest| rest.split(',').next())
        .map(str::trim)
        .expect("the banner names an address")
        .to_string();
    assert!(
        !addr.ends_with(":0"),
        "the banner printed the REQUESTED port, not the bound one ({addr}) — this test cannot \
         reach the daemon, and neither can anything else that started it on port 0"
    );

    // Distinctive enough that a leak cannot be a coincidence.
    let source = "fn proprietary_edge_7f3a() {} // the caller's own code";
    let keys = NodeKeys::new(Vec::new(), key.as_bytes().to_vec());
    let answer = build_remote(&addr, &keys, "journal_probe", source, "");
    match &answer {
        Err(BuildRequestError::Compile(diagnostics)) => assert!(
            diagnostics.contains("source version mismatch"),
            "the peer was answered, but not with the version refusal this test planted: \
             {diagnostics}"
        ),
        other => panic!("expected the version refusal as a BuildErr, got {other:?}"),
    }

    // THE PROPERTY: the same refusal, on stderr — where systemd's journal reads it.
    let line =
        wait_for(&rx, Duration::from_secs(10), |l| l.contains("REFUSED")).unwrap_or_else(|seen| {
            panic!(
                "the peer was refused and the daemon's stderr said NOTHING about it — the \
                 journal of a daemon refusing every request reads like an idle one. Lines seen \
                 after the banner: {seen:#?}"
            )
        });
    // Printed so a `--nocapture` run shows the exact line an operator's journal carries.
    eprintln!("the daemon's journal line: {line}");
    assert!(line.contains("[source-version-mismatch]"), "the line names no reason: {line}");
    assert!(line.contains(stale_stamp), "the line must name the tree's stamp: {line}");
    if vike_buildinfo::GIT_SHA != vike_buildinfo::UNKNOWN {
        assert!(
            line.contains(vike_buildinfo::GIT_SHA),
            "the line must name the binary's own commit: {line}"
        );
    }
    assert!(!line.contains("proprietary_edge_7f3a"), "the SOURCE reached the journal: {line}");
    assert!(!line.contains(key), "the KEY reached the journal: {line}");
}

/// A `WORKSPACE_ROOT_ENV` naming a tree that carries its own `settings/` must refuse at STARTUP —
/// before the listener even binds — because the jail binds that tree back READ-ONLY into every
/// compile, and a `settings/`-bearing tree is exactly the venue credential store this branch's
/// filesystem jail exists to hide. Proven against the real binary: no "listening on" banner, a
/// non-zero exit, and the planted key never on stderr.
#[test]
fn a_workspace_root_carrying_settings_refuses_before_the_listener_binds() {
    let root = tempfile::tempdir().expect("tempdir");
    let out = tempfile::tempdir().expect("tempdir");
    // The shape a real venue credential store takes — the content is never read by the check
    // (`workspace_root.join("settings").is_dir()`), only its presence, so an empty directory
    // proves the refusal is not silently keyed on the store having real contents.
    std::fs::create_dir_all(root.path().join("settings")).expect("plant settings/");

    let key = "refusal-test-key-not-a-real-credential";
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-strategy-builder"))
        .env(BUILDER_KEY_ENV, key)
        .env(WORKSPACE_ROOT_ENV, root.path())
        .env("VIKE_STRATEGY_BUILDER_PORT", "0")
        .env("VIKE_STRATEGY_BUILDER_OUT_DIR", out.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the builder binary");
    let rx = stderr_lines(&mut child);

    let status = child.wait().expect("wait on the refused daemon");
    assert!(
        !status.success(),
        "a settings-bearing workspace_root must refuse, not start: {status:?}"
    );

    let mut lines = Vec::new();
    while let Ok(line) = rx.recv_timeout(Duration::from_secs(5)) {
        lines.push(line);
    }
    assert!(
        lines.iter().any(|l| l.contains("refusing to start")),
        "no startup refusal on stderr. Lines seen: {lines:#?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("listening on")),
        "the daemon bound a socket before refusing a settings-bearing workspace_root: {lines:#?}"
    );
    // The refusal is printed as SEVERAL lines (`eprintln!`'s embedded `\n`s), so `stderr_lines`
    // (which splits on newlines the same way a journal would) hands them back one line at a time.
    // Join them back before searching, or a check for a word on line 2 of a 3-line message reads
    // only line 1 and can never pass — the exact shape this line first shipped with.
    let refusal = lines.join("\n");
    assert!(refusal.contains("settings"), "the refusal must name what it found: {refusal}");
    assert!(!refusal.contains(key), "the KEY reached the refusal: {refusal}");
}
