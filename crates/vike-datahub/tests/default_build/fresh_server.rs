//! **A fresh server's datahub SERVES with nothing to record, and restarts itself to record the first recorder
//! profile** (the owner, 2026-11-16: an end user never runs a setup step), over the SHIPPED binary started exactly
//! as the server install starts it (`--recorder-profile default`) on a settings database that holds no profile.
//!
//! Before the change this start exited 78 ("setup needed: run `vike-cli config profile create recorder`") and its
//! supervisor parked it. Now it listens; a SIGTERM still stops it gracefully (the recording's stop handlers are
//! armed, so the serve loop runs on a thread of its own), and a recorder profile stored while it serves ends it with
//! the restart status, so the next start records.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use vike_log::reaper::Reaper;
use vike_secrets::profile_store::{
    ProfileKind, ProfileOperatorWrite, ProfileRecord, ProfileRow, store_profile,
};

/// How long each awaited fact may take on a loaded box.
const PATIENCE: Duration = Duration::from_secs(60);

/// A fresh project whose datahub listens on a free loopback port: its settings database with that row and nothing
/// else. Returns the project and the address.
fn fresh_project() -> (tempfile::TempDir, String) {
    let root = tempfile::tempdir().expect("a scratch project");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a loopback port")
        .port();
    let addr = format!("127.0.0.1:{port}");
    vike_secrets::plant_settings_rows(
        &root.path().join("settings"),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_bind_addr".to_string(),
                value: format!("\"{addr}\""),
            }],
            ..Default::default()
        },
    )
    .expect("plant the store and the bind row");
    (root, addr)
}

/// Start the shipped datahub as the server install does, in `project`; its stderr collected.
fn start(project: &Path) -> (Reaper, Arc<Mutex<String>>) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_vike-datahub"));
    command
        .args(["--recorder-profile", "default"])
        .current_dir(project)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .env("VIKE_SETTINGS_DIR", project.join("settings"))
        .env("VIKE_LOG_DIR", project.join("logs"))
        .env_remove("RUST_LOG")
        .env_remove("VIKE_LOG");
    for r in vike_config::REMOVED_ENV {
        command.env_remove(r.var);
    }
    let mut child = Reaper(command.spawn().expect("spawn vike-datahub"));
    let log = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&log);
    let err_pipe = child.0.stderr.take().expect("piped stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(err_pipe).lines().map_while(Result::ok) {
            let mut buffer = sink.lock().expect("stderr buffer");
            buffer.push_str(&line);
            buffer.push('\n');
        }
    });
    (child, log)
}

/// Wait for the datahub at `addr` to accept a connection and for its stderr to say why it records nothing, failing
/// with its log if it exits first.
fn await_listening(child: &mut Reaper, addr: &str, log: &Mutex<String>) {
    let mut exited = None;
    let up = vike_log::poll::wait_until(PATIENCE, || {
        exited = child.0.try_wait().expect("try_wait");
        exited.is_some() || TcpStream::connect(addr).is_ok()
    });
    // The datahub logs the line BEFORE it binds, but the reader thread above may not have buffered it by the moment
    // the connect succeeds: wait for it rather than read one snapshot.
    if up && exited.is_none() {
        vike_log::poll::wait_until(PATIENCE, || {
            exited = child.0.try_wait().expect("try_wait");
            exited.is_some() || log.lock().expect("stderr buffer").contains("NOTHING TO RECORD YET")
        });
    }
    let log = log.lock().expect("stderr buffer").clone();
    assert!(exited.is_none(), "the datahub EXITED ({exited:?}) on a fresh server; stderr:\n{log}");
    assert!(up, "the datahub did not listen at {addr} within {PATIENCE:?}; stderr:\n{log}");
    assert!(
        log.contains("NOTHING TO RECORD YET"),
        "it says why it records nothing; stderr:\n{log}"
    );
}

/// The exit status of `child` once it ends, within [`PATIENCE`].
fn exit_code(child: &mut Reaper, log: &Mutex<String>) -> Option<i32> {
    let mut exited = None;
    let ended = vike_log::poll::wait_until(PATIENCE, || {
        exited = child.0.try_wait().expect("try_wait");
        exited.is_some()
    });
    assert!(ended, "the datahub did not end; stderr:\n{}", log.lock().expect("stderr buffer"));
    exited.and_then(|s| s.code())
}

#[test]
fn a_fresh_server_serves_with_nothing_to_record_and_a_sigterm_stops_it() {
    let (root, addr) = fresh_project();
    let (mut child, log) = start(root.path());
    await_listening(&mut child, &addr, &log);

    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.0.id().to_string())
        .status()
        .expect("run kill(1)");
    assert!(status.success(), "kill -TERM failed: {status:?}");
    assert_eq!(exit_code(&mut child, &log), Some(0), "a stop is a clean exit");
}

#[test]
fn the_first_recorder_profile_stored_restarts_it_to_record() {
    let (root, addr) = fresh_project();
    let (mut child, log) = start(root.path());
    await_listening(&mut child, &addr, &log);

    let db = vike_secrets::db_path_in(&root.path().join("settings"));
    let profile = ProfileRecord {
        row: ProfileRow {
            name: "default".to_string(),
            kind: ProfileKind::Recorder,
            active: false,
            note: None,
        },
        mounts: Vec::new(),
        params: BTreeMap::new(),
        settings: BTreeMap::new(),
        recorder: None,
    };
    let write = ProfileOperatorWrite::claim("vike-datahub fresh-server test");
    store_profile(&db, &profile, &write, 0, vike_model::AssetClass::SQL_WORDS)
        .expect("store a recorder profile");
    assert_eq!(
        exit_code(&mut child, &log),
        Some(i32::from(vike_utility::cli::PLANNED_RESTART_EXIT)),
        "the restart status, so the next start records"
    );
}
