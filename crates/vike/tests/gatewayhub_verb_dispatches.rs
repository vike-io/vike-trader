//! `vike-backend gatewayhub` reaches REAL code, proven by running the compiled multicall binary as a
//! subprocess rather than by reading `TOOLS` and trusting it.
//!
//! This is `strategy_builder_verb_dispatches.rs`'s twin, for the gateway daemon that
//! `deploy/vike-gatewayhub.service` starts as `bin/vike-backend gatewayhub`. A misnamed or missing row
//! is `Route::Usage(2)`: exit 2 with the dispatcher's tool list, a daemon that "started" and did
//! nothing. Every case below tells that apart from the daemon's own answer.
//!
//! ⚠ Every invocation here is one the daemon answers BEFORE it boots (`--help`, `--version`, a usage
//! error), so none of them opens a store, binds a port or reads a settings directory — which is what
//! lets them run on any box, and why none sets `VIKE_SETTINGS_DIR`. The boot itself is the crate's own
//! `tests/help_cli.rs` against its standalone binary.
//!
//! ⚠ Whole-file gated on the `vike-gatewayhub` feature, the same switch `crates/vike/Cargo.toml`'s row
//! is behind: a default build of `vike-backend` has no `gatewayhub` row at all. `cargo test -p
//! vike-backend --features vike-gatewayhub` (or any lane that turns the feature on, `full` included)
//! is what runs it.
#![cfg(feature = "vike-gatewayhub")]

use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long an invocation that must ANSWER may take. Enormous for what it measures: it is the line
/// between "exited" and "is serving", and a regression that made `--help` start the daemon must fail
/// here in 30 s with a diagnosis rather than park a CI job.
const EXIT_DEADLINE: Duration = Duration::from_secs(30);

/// Run `vike-backend <args>` and REQUIRE it to exit.
fn run(args: &[&str]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-backend"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn vike-backend {args:?}: {e}"));
    let deadline = Instant::now() + EXIT_DEADLINE;
    loop {
        match child.try_wait().expect("poll the child") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!(
                    "`vike-backend {}` did not exit within {EXIT_DEADLINE:?} — it is still RUNNING, \
                     which for this tool means it started the daemon instead of answering",
                    args.join(" ")
                );
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect the exited child's output")
}

/// The dispatcher's own usage text opens with this phrase; the daemon's never does.
const DISPATCHER_USAGE: &str = "vike-backend — one executable";

/// The multicall's `--help` NAMES the verb — the text an operator reads before ever typing it.
#[test]
fn the_tool_list_names_gatewayhub() {
    let out = run(&["--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("gatewayhub"),
        "vike-backend --help does not list gatewayhub — TOOLS table row missing or the feature was \
         not compiled in for this binary. Full output:\n{stdout}"
    );
}

/// `vike-backend gatewayhub --help` is the DAEMON's page: exit 0, usage on stdout, the settings row
/// it reads named — and not the dispatcher's tool list, which is what a missing row would print (and
/// exit 2 with, on stderr).
#[test]
fn the_verb_reaches_the_daemons_own_help() {
    let out = run(&["gatewayhub", "--help"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "`gatewayhub --help` must exit 0; stderr: {stderr}");
    assert!(stdout.contains("Usage:"), "the daemon prints its usage on STDOUT: {stdout:?}");
    assert!(
        stdout.contains("config.gatewayhub_addr"),
        "the page must name the row the daemon binds from — this is the daemon's help, not the \
         dispatcher's: {stdout:?}"
    );
    assert!(
        !stdout.contains(DISPATCHER_USAGE),
        "`gatewayhub --help` printed the DISPATCHER'S usage — the row did not resolve to a real tool: \
         {stdout:?}"
    );
}

/// `--version` is answered by the daemon too, and names the daemon.
#[test]
fn the_verb_answers_version() {
    let out = run(&["gatewayhub", "--version"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "`gatewayhub --version` must exit 0");
    assert!(stdout.contains("vike-gatewayhub"), "the daemon's own identity line: {stdout:?}");
}

/// A typo on the unit's `ExecStart=` line is REFUSED by the daemon's parser, naming the argument, with
/// exit 2 on stderr — never ignored, and not the dispatcher's usage either. The distinction is the
/// whole point: both exit 2, and only the first proves the verb dispatches to the daemon.
#[test]
fn an_unknown_flag_is_the_daemons_refusal_not_the_dispatchers() {
    let out = run(&["gatewayhub", "--adr=127.0.0.1:7883"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "a refused command line exits 2; stderr: {stderr:?}");
    assert!(stderr.contains("--adr"), "the refusal names the offending argument: {stderr:?}");
    assert!(
        !stdout.contains(DISPATCHER_USAGE) && !stderr.contains(DISPATCHER_USAGE),
        "the dispatcher's own usage text appeared — that is `Route::Usage(2)`, meaning the link did \
         not resolve to a real tool. stdout: {stdout:?} stderr: {stderr:?}"
    );
}
