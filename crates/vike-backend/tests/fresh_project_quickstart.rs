//! **A fresh project's documented first run READS**: the steps `vike-cli init` prints before its datahub line, run
//! in their printed order through this package's own multicall in a throwaway project, then the datahub, then a
//! history read that datahub ADMITS.
//!
//! Every hub always authenticates against the project's access list (decision 0288 item 8), and an empty list
//! refuses every read as "no device registered". Until 2026-11-21 the printed steps (and the public README's,
//! `scripts/mirror_readme.md`) started the datahub straight after `init`, so a new user's first read was refused
//! (measured on a the latency box lane). The steps are read off `init`'s own stdout, so an edit that drops the device step
//! from what a user is told reddens here, not on a user's box.
//!
//! ⚠ The ONE deviation from the printed steps: the datahub binds a FREE loopback port (`config.datahub_bind_addr`,
//! and `config.datahub_addr` for the client), never the default 7878, because CI runs beside the CI box's live hubs.
//!
//! ⚠ Whole-file gated on the two tools it runs; the self dev-dependency's `full` turns both on.
#![cfg(all(feature = "vike-cli", feature = "vike-datahub"))]

use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long the datahub may take to listen on a loaded box.
const PATIENCE: Duration = Duration::from_secs(60);

/// `vike-backend` in `project`, aimed at its settings and logging inside it.
fn backend(project: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vike-backend"));
    command
        .current_dir(project)
        .stdin(Stdio::null())
        .env("VIKE_SETTINGS_DIR", project.join("settings"))
        .env("VIKE_LOG_DIR", project.join("logs"))
        .env_remove("RUST_LOG")
        .env_remove("VIKE_LOG");
    command
}

/// `vike-cli <args>` through the multicall, run to its end.
fn cli(project: &Path, args: &[&str]) -> Output {
    backend(project).arg("vike-cli").args(args).output().expect("run vike-backend vike-cli")
}

fn said(out: &Output) -> String {
    format!(
        "exit {:?}\nstdout:\n{}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// The command lines `init` printed, each as its tokens (a `# note` dropped), in printed order.
fn printed_commands(stdout: &str) -> Vec<Vec<String>> {
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .filter(|line| line.starts_with("vike-cli ") || line.starts_with("vike-backend "))
        .map(|line| {
            let command = line.split(" # ").next().unwrap_or_default();
            command.split_whitespace().map(str::to_string).collect()
        })
        .collect()
}

/// The datahub, killed when dropped.
struct Datahub(Child);

impl Drop for Datahub {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn the_printed_first_run_steps_give_a_fresh_project_a_datahub_that_admits_its_read() {
    let scratch = tempfile::tempdir().expect("a scratch folder");
    let project = scratch.path();
    std::fs::create_dir_all(project.join("settings")).expect("settings/ makes it a project");

    let init = cli(project, &["init"]);
    assert!(init.status.success(), "init: {}", said(&init));
    let printed = printed_commands(&String::from_utf8_lossy(&init.stdout));
    let datahub_line = printed
        .iter()
        .position(|argv| argv == &["vike-backend", "datahub"])
        .unwrap_or_else(|| panic!("init prints no `vike-backend datahub` line: {printed:?}"));
    let first_steps = &printed[..datahub_line];
    assert!(
        first_steps.iter().any(|argv| argv.join(" ") == "vike-cli backend mint-missing"),
        "init must tell a fresh project to register this box's device before its datahub: {first_steps:?}"
    );
    for argv in first_steps {
        assert_eq!(argv[0], "vike-cli", "a step before the datahub this test cannot run: {argv:?}");
        let words: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
        let out = cli(project, &words);
        assert!(out.status.success(), "`{}`: {}", argv.join(" "), said(&out));
    }

    // The one deviation: a free port, never 7878 (the CI box's live datahub on a CI runner).
    let port = TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a free loopback port")
        .port();
    let addr = format!("127.0.0.1:{port}");
    for key in ["config.datahub_bind_addr", "config.datahub_addr"] {
        let out = cli(project, &["config", "set", key, &addr]);
        assert!(out.status.success(), "config set {key}: {}", said(&out));
    }

    let log = std::fs::File::create(project.join("datahub.log")).expect("a log file");
    let child = backend(project)
        .arg("datahub")
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .expect("start vike-backend datahub");
    let mut hub = Datahub(child);
    let started = Instant::now();
    while TcpStream::connect(&addr).is_err() {
        let exited = hub.0.try_wait().expect("poll the datahub");
        let log = std::fs::read_to_string(project.join("datahub.log")).unwrap_or_default();
        assert!(exited.is_none(), "the datahub exited ({exited:?}):\n{log}");
        assert!(started.elapsed() < PATIENCE, "the datahub did not listen at {addr}:\n{log}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let read = cli(project, &["data", "history", "ls"]);
    assert!(read.status.success(), "the first read was refused: {}", said(&read));

    // The kill proof: the read above was admitted BY the access list, not by a datahub that admits anyone.
    let revoke = cli(project, &["access", "revoke", "owner/server"]);
    assert!(revoke.status.success(), "access revoke: {}", said(&revoke));
    let refused = cli(project, &["data", "history", "ls"]);
    assert!(!refused.status.success(), "a revoked device still read: {}", said(&refused));
}
