//! **A tradehub whose every account waits on a dead gatewayhub does not exit** (owner 2026-11-14: never a restart every
//! 5 s), proved against the REAL binary: the live gate on, one bybit `demo` account, a gatewayhub Trade key, and a
//! `config.gatewayhub_addr` row naming a loopback port nothing listens on.
//!
//! Before the change this start waited `config.gatewayhub_connect_wait_ms`, assembled no engine
//! (`RuntimeError::NoAccountRuns`) and exited 1 within seconds, for its supervisor to restart into the same answer.
//! Now it WAITS before the assembly (`src/tradehub_cli/hub_wait.rs`), saying `"kind":"starting"` with
//! `waiting for the gatewayhub at <addr>` on its protocol stream (the door's `starting` words), asking again on the
//! 5 s to 30 s ladder, and a SIGTERM ends the wait. Pinned on the old code first: there the child exits 1 and the
//! starting line never comes.
//!
//! Unix only: the stop is a POSIX signal (`sigterm_stop.rs`'s reason; the stdin `shutdown` word is read only once the
//! node runs, and the start never got that far).

#![cfg(unix)]

use crate::daemon_profile_seed;
use crate::temp_project;

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vike_log::reaper::Reaper;

use temp_project::temp_dir;

/// How long to wait for the starting line, and for the exit after the signal.
const PATIENCE: Duration = Duration::from_secs(30);

/// Past this, the old start had long exited: two pauses of the ladder's first step (`late_arm::FIRST_POLL`, 5 s).
const ALIVE_AFTER: Duration = Duration::from_secs(12);

#[test]
fn a_tradehub_whose_every_account_waits_on_a_dead_gatewayhub_does_not_exit_and_says_so() {
    let root = temp_dir("hub-wait");
    let dir = root.path();
    let settings = dir.join("settings");
    // A port nothing listens on: bound, read, released.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|l| l.local_addr())
        .expect("a loopback port")
        .port();
    let addr = format!("127.0.0.1:{port}");
    // The live gate and the gatewayhub's address are ROWS (decision 0111). Planted FIRST: this replaces every
    // settings table.
    vike_secrets::plant_settings_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: [
                ("flags", "tradehub_live", "true".to_string()),
                ("config", "gatewayhub_addr", format!("\"{addr}\"")),
                ("config", "gatewayhub_connect_wait_ms", "1000".to_string()),
            ]
            .into_iter()
            .map(|(section, key, value)| vike_secrets::SettingRow {
                section: section.to_string(),
                key: key.to_string(),
                value,
            })
            .collect(),
            ..Default::default()
        },
    )
    .expect("plant the rows");
    daemon_profile_seed::seed_active_daemon_profile(
        &settings,
        "hub-wait",
        "venue = \"bybit\"\n\
         asset_class = \"CryptoPerp\"\n\
         symbol = \"BTCUSDT\"\n\
         interval = \"1m\"\n\
         interval_ms = 60000\n\
         tick_size = 0.1\n\
         \n\
         [strategy]\n\
         file = \"buy_hold.rhai\"\n\
         \n\
         [strategy.params]\n\
         size = 0.001\n\
         \n\
         [daemon]\n\
         summary_ms = 1000\n\
         shutdown_deadline_ms = 5000\n",
    );
    // The one account: bybit's DEFAULT account at `demo`, stating the mounted symbol. It runs nothing without the
    // gatewayhub, and no `paper` account stands beside it.
    let created = vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::Create {
            venue: "bybit",
            tier: vike_model::AccountTier::Demo,
            label: None,
        },
    )
    .expect("a bybit demo row");
    let id = created.after.expect("a created row").id;
    vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::SetSymbol { id, symbol: Some("BTCUSDT") },
    )
    .expect("state the symbol");
    // The tradehub's Trade key for the gatewayhub: with none, nothing would be waited for (setup is refused).
    vike_secrets::save_credentials_to_store_unjournalled(
        &settings,
        vike_secrets::StoreTable::HubKey,
        &[(
            vike_venues::credential_keys::gatewayhub_trade_key().to_string(),
            "hub-wait-test-trade-key".to_string(),
        )],
        vike_secrets::CredentialUpsertIntent::Add,
    )
    .expect("file the Trade key");

    let mut command = Command::new(env!("CARGO_BIN_EXE_vike-tradehub"));
    command
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("VIKE_SETTINGS_DIR", &settings)
        .env_remove("VIKE_STATE_ROOT")
        .env("VIKE_LOG_DIR", dir.join("logs"))
        .env_remove("VIKE_LOG_FILE_LEVEL")
        .env_remove("RUST_LOG")
        .env_remove("VIKE_LOG");
    // A variable this build REFUSES must not leak in from the runner's shell and end the start on a refusal this
    // test is not about.
    for r in vike_config::REMOVED_ENV {
        command.env_remove(r.var);
    }
    let started = Instant::now();
    let mut child = Reaper(command.spawn().expect("spawn vike-tradehub"));

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

    let wanted = format!("waiting for the gatewayhub at {addr}");
    let deadline = Instant::now() + PATIENCE;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match lines.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) if line.contains("\"kind\":\"starting\"") && line.contains(&wanted) => break,
            Ok(_) => {}
            Err(RecvTimeoutError::Timeout) => {
                panic!("no `{wanted}` starting line within {PATIENCE:?}; stderr:\n{}", log())
            }
            Err(RecvTimeoutError::Disconnected) => {
                let status = child.0.wait().expect("wait");
                panic!(
                    "the tradehub EXITED ({status}) instead of waiting for the gatewayhub at {addr}: the restart \
                     loop this test guards against. stderr:\n{}",
                    log()
                )
            }
        }
    }

    std::thread::sleep(ALIVE_AFTER.saturating_sub(started.elapsed()));
    assert!(
        child.0.try_wait().expect("try_wait").is_none(),
        "the tradehub must still be waiting {ALIVE_AFTER:?} after its start; stderr:\n{}",
        log()
    );

    let status = Command::new("kill")
        .arg("-TERM")
        .arg(child.0.id().to_string())
        .status()
        .expect("run kill(1)");
    assert!(status.success(), "kill -TERM failed: {status:?}");
    let mut exited = None;
    let ended = vike_log::poll::wait_until(PATIENCE, || {
        exited = child.0.try_wait().expect("try_wait");
        exited.is_some()
    });
    assert!(ended, "a stop must end the wait within {PATIENCE:?}; stderr:\n{}", log());
    assert!(
        log().contains("stopped while waiting for the gatewayhub"),
        "the stop names the wait it ended; stderr:\n{}",
        log()
    );
}
