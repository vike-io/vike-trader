//! The server INSTALLS the user's indicators at startup — asserted over the SHIPPED binary, because
//! the claim is about a call site in `main` and nothing else can see one.
//!
//! ⚠ **Why this could not be folded into `help_cli.rs`.** `--help` and `--version` are answered
//! before anything else (that is what `help_cli.rs` exists to pin), so they short-circuit ABOVE the
//! install and prove nothing about it. Deleting `install_user_indicators()` from `main` leaves every
//! other test in the tree green — which is exactly the gap this file closes.
//!
//! ⚠ **The binary is made to FAIL AFTER installing.** `install_user_indicators` runs after
//! `vike_log::init` and before the `TcpListener` bind, so an unbindable address gives a run that
//! reaches the install, reports what it found, and then exits non-zero — an exit this test needs,
//! since a successful start would serve forever. The address is deliberately malformed rather than
//! merely in use: "already bound" depends on what else is running on the box, and a test whose
//! premise is another process is a flake.
//!
//! Feature-gated because the install is: a default (DataFusion-free) build compiles no serving
//! `main` at all, so there is no call site to assert. `cargo test -p vike-datahub --features
//! serve-datafusion` is the lane that runs it.

#![cfg(feature = "serve-datafusion")]

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const EXIT_DEADLINE: Duration = Duration::from_secs(30);

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("vike-datahub-ind-{tag}-{nanos}"));
        std::fs::create_dir_all(p.join("indicators")).expect("scratch");
        Self(p)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Run the shipped binary with a controlled environment and REQUIRE it to exit. Returns
/// `(status_success, stdout + stderr)` — both streams merged, because which one a `tracing`
/// subscriber writes to is a property of `vike_log`'s configuration and not of the thing under test.
fn run_with(user_data: &Path, addr: &str) -> (bool, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-datahub"))
        .env("VIKE_USER_DATA_DIR", user_data)
        .env("VIKE_DATAHUB_ADDR", addr)
        // Keep the console layer permissive: a default filter that dropped `warn` would make this
        // test measure the log level rather than the install.
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vike-datahub");

    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "vike-datahub did not exit within {EXIT_DEADLINE:?} — it bound a listener \
                     despite a malformed address, so this test can no longer make it fail after \
                     the install"
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().expect("collect output");
    let mut merged = String::from_utf8_lossy(&out.stdout).into_owned();
    merged.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.success(), merged)
}

/// ⚠ **The gate for the call site itself.** A broken indicator in the server's own directory must be
/// REPORTED by the server at startup — which can only happen if `main` actually calls the install.
///
/// Non-vacuous by construction: with `install_user_indicators()` deleted from `main`, the binary
/// never opens the directory, so neither the rejection nor the installed-set line can appear and
/// both assertions below fail. It cannot pass by accident either — the file name it looks for is
/// unique to this scratch directory.
#[test]
fn the_server_installs_the_user_indicator_directory_and_reports_what_it_rejected() {
    let s = Scratch::new("reject");
    // No `fn on_bar(bar)` — a compile failure the loader reports by file.
    std::fs::write(s.path().join("indicators/half_edited.rhai"), "fn init() { #{} }").unwrap();
    // ...and one that loads, so the run proves BOTH halves: a rejection is reported and a good
    // sibling still installs.
    std::fs::write(s.path().join("indicators/works.rhai"), "fn on_bar(bar) { bar.close }").unwrap();

    let (ok, out) = run_with(s.path(), "not-an-address");
    assert!(!ok, "a malformed listen address must still fail the run: {out}");
    assert!(
        out.contains("half_edited.rhai"),
        "the server must name the file it could not load — otherwise a script author has no way to \
         learn why their call resolved to nothing:\n{out}"
    );
    assert!(
        out.contains("works"),
        "...and it must report the set it DID install, which is the only record that lets an \
         operator diagnose a server-vs-server divergence:\n{out}"
    );
}

/// A directory with no `indicators/` at all is the ordinary state and must be silent about
/// rejections — a fresh server is not broken. It still reports its (empty) set, which is what
/// distinguishes "nothing to install" from "never looked".
#[test]
fn a_server_with_no_indicators_reports_an_empty_set_rather_than_saying_nothing() {
    let s = Scratch::new("empty");
    let (ok, out) = run_with(s.path(), "not-an-address");
    assert!(!ok);
    assert!(!out.contains("indicator not loaded"), "an empty directory is not a rejection:\n{out}");
    assert!(
        out.contains("user indicators installed"),
        "the install line must appear even when the set is empty:\n{out}"
    );
}
