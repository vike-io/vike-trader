//! **A fresh server's tradehub comes up EMPTY and a device registers through its door** (the owner, 2026-11-16: an
//! end user never runs a setup step), proved against the REAL binary over a settings database that holds what a
//! first start leaves (the door's address row, an invitation) and no daemon profile and no account.
//!
//! Before the change this start exited 78 ("setup needed: run `vike-cli config profile create daemon`") and its supervisor
//! parked it, so no door was up for a guest to register through. Now it prints a ready line led by
//! `NO DAEMON PROFILE`, a guest's device registers with the invitation and reads a frame whose identity says so, and
//! once a daemon profile is stored ACTIVE the node stops by itself with the planned-restart status (75).

use crate::daemon_profile_seed;
use crate::temp_project::temp_dir;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_hub_protocol::access::{DeviceKey, invite_auth_secret};
use vike_log::reaper::Reaper;
use vike_tradehub_client::{register_device, snapshot_once};

/// How long each awaited fact may take on a loaded box.
const PATIENCE: Duration = Duration::from_secs(60);

/// A guest's permanent ssh key (it lands in `authorized_keys` only at the swap, which this test does not run).
const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBeY8sZr3o6R0x8T1v9Kc2QmN4yXh7pJwLfGd5uVbE0a";

#[test]
fn a_fresh_server_starts_empty_a_device_registers_and_the_first_profile_restarts_it() {
    let root = temp_dir("empty-node");
    let dir = root.path();
    let settings = dir.join("settings");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a loopback port")
        .port();
    let addr = format!("127.0.0.1:{port}");
    // What a first start leaves: the store and the door's row (`backend mint-missing` writes it), nothing else.
    vike_secrets::plant_settings_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "tradehub_addr".to_string(),
                value: format!("\"{addr}\""),
            }],
            ..Default::default()
        },
    )
    .expect("plant the store and the door's row");
    assert!(
        vike_secrets::access::add_access_invite(
            &settings,
            "0a1b2c3d",
            "alice",
            "observe",
            &invite_auth_secret("s3cret"),
            i64::MAX,
        )
        .expect("the invitation is written"),
        "a fresh invitation"
    );

    let mut command = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"));
    command
        .current_dir(dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("VIKE_SETTINGS_DIR", &settings)
        .env_remove("VIKE_STATE_ROOT")
        .env("VIKE_LOG_DIR", dir.join("logs"))
        .env_remove("VIKE_LOG_FILE_LEVEL")
        .env_remove("RUST_LOG")
        .env_remove("VIKE_LOG");
    for r in vike_config::REMOVED_ENV {
        command.env_remove(r.var);
    }
    let mut child = Reaper(command.spawn().expect("spawn vike-tradehub"));
    // Held open for the child's life: a closed stdin is EOF, which a non-tty run only logs.
    let _stdin = child.0.stdin.take().expect("piped stdin");

    let (tx, lines) = mpsc::channel();
    let stdout = child.0.stdout.take().expect("piped stdout");
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let stderr = Arc::new(Mutex::new(String::new()));
    let sink = Arc::clone(&stderr);
    let err_pipe = child.0.stderr.take().expect("piped stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(err_pipe).lines().map_while(Result::ok) {
            let mut buffer = sink.lock().expect("stderr buffer");
            buffer.push_str(&line);
            buffer.push('\n');
        }
    });
    let log = || stderr.lock().expect("stderr buffer").clone();

    let deadline = Instant::now() + PATIENCE;
    let ready = loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) if line.contains("\"kind\":\"ready\"") => break line,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("no ready line within {PATIENCE:?}; stderr:\n{}", log())
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = child.0.wait().expect("wait");
                panic!(
                    "the tradehub EXITED ({status}) on a fresh server instead of starting empty; stderr:\n{}",
                    log()
                )
            }
        }
    };
    assert!(ready.contains("NO DAEMON PROFILE"), "the ready line says why nothing runs: {ready}");

    // The guest's device registers with the invitation, then reads under its role.
    let (device, _) = DeviceKey::generate().expect("a device key");
    let mut granted = None;
    let registered = vike_log::poll::wait_until(PATIENCE, || {
        granted =
            register_device(addr.as_str(), "0a1b2c3d", "s3cret", "macbook", &device, SSH_KEY).ok();
        granted.is_some()
    });
    assert!(registered, "the door registered no device within {PATIENCE:?}; stderr:\n{}", log());
    let granted = granted.expect("registered");
    assert_eq!((granted.person.as_str(), granted.role.as_str()), ("alice", "observe"));
    let frame = snapshot_once(addr.as_str(), &device).expect("the registered device reads a frame");
    let identity = frame.identity.expect("the empty node names itself");
    assert_eq!(identity.name, "NO DAEMON PROFILE", "{identity:?}");
    assert!(frame.venues.is_empty(), "the empty node runs no account: {:?}", frame.venues);

    // The first ACTIVE daemon profile stops the empty node with the planned-restart status, for its supervisor.
    daemon_profile_seed::seed_active_daemon_profile(
        &settings,
        "first",
        "venue = \"bybit\"\nasset_class = \"CryptoPerp\"\nsymbol = \"BTCUSDT\"\ninterval = \"1m\"\n\
         interval_ms = 60000\ntick_size = 0.1\n\n[strategy]\nfile = \"buy_hold.rhai\"\n",
    );
    let mut exited = None;
    let ended = vike_log::poll::wait_until(PATIENCE, || {
        exited = child.0.try_wait().expect("try_wait");
        exited.is_some()
    });
    assert!(ended, "the empty node did not stop for its first profile; stderr:\n{}", log());
    assert_eq!(
        exited.and_then(|s| s.code()),
        Some(i32::from(vike_utility::cli::PLANNED_RESTART_EXIT)),
        "stderr:\n{}",
        log()
    );
}
