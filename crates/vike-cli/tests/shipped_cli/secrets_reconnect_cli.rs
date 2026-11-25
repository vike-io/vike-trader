//! **`secrets set` tells the running gatewayhub, through the datahub, and nothing but the account's id travels**
//! (decision 0287), over the SHIPPED binary: a real datahub server on loopback whose relay to the gatewayhub is
//! scripted, the `config.datahub_addr` row aimed at it, the value piped on stdin. The relay must be asked for the
//! account the write landed on, and the verb must say what the gatewayhub did; with no datahub answering it must say
//! the change takes effect at the next start, and exit clean either way (the write has landed).

use std::net::{SocketAddr, TcpListener};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub::server::{CredentialsRelay, ServerLanes, ServerTimings, serve_with_lanes};
use vike_datahub_client::gateway_status::CredentialsChangedReply;

const VALUE: &str = "piped-secret-value-never-printed";

/// A key-less loopback datahub whose relay records every account it is asked about and answers a reconnect.
fn datahub_relaying(asked: Arc<Mutex<Vec<i64>>>) -> SocketAddr {
    let relay: CredentialsRelay = Arc::new(move |account| {
        asked.lock().unwrap_or_else(PoisonError::into_inner).push(account);
        CredentialsChangedReply { reached: true, reconnected: vec!["okx".to_string()], error: None }
    });
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let lanes = ServerLanes { credentials_relay: Some(relay), ..ServerLanes::default() };
        let _ = serve_with_lanes(
            listener,
            store,
            None,
            None,
            None,
            None,
            lanes,
            None,
            ServerTimings::PRODUCTION,
        );
    });
    addr
}

/// A project with one okx demo account and the `config.datahub_addr` row naming `datahub`: its settings dir and the
/// account's id.
fn project(datahub: &str) -> (tempfile::TempDir, std::path::PathBuf, i64) {
    let root = tempfile::tempdir().expect("tempdir");
    let settings = root.path().join("settings");
    std::fs::create_dir_all(&settings).expect("create the settings directory");
    vike_secrets::create_store(Some(settings.to_str().expect("a UTF-8 path"))).expect("the store");
    let id = vike_secrets::edit_account_in_unjournalled(
        &settings,
        vike_secrets::AccountEdit::Create {
            venue: "okx",
            tier: vike_model::AccountTier::Demo,
            label: None,
        },
    )
    .expect("create the account")
    .after
    .expect("a create returns its row")
    .id;
    vike_secrets::plant_settings_rows(
        &settings,
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "config".to_string(),
                key: "datahub_addr".to_string(),
                value: format!("\"{datahub}\""),
            }],
            ..Default::default()
        },
    )
    .expect("plant the datahub_addr row");
    (root, settings, id)
}

/// `secrets set --id ID API_KEY` with the value on stdin: its exit code, stdout and stderr.
fn set_api_key(settings: &std::path::Path, id: i64) -> (Option<i32>, String, String) {
    use std::io::Write as _;
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["secrets", "set", "--id", &id.to_string(), "API_KEY"])
        .env("VIKE_SETTINGS_DIR", settings)
        .env_remove("VIKE_USER_DATA_DIR")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vike-cli");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(format!("{VALUE}\n").as_bytes())
        .expect("pipe the value");
    let out = child.wait_with_output().expect("vike-cli ends");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

#[test]
fn a_landed_write_tells_the_running_gatewayhub_the_account_id_alone() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let datahub = datahub_relaying(Arc::clone(&asked)).to_string();
    let (_root, settings, id) = project(&datahub);
    let (code, stdout, stderr) = set_api_key(&settings, id);
    assert_eq!(code, Some(0), "--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}");
    assert_eq!(
        *asked.lock().unwrap_or_else(PoisonError::into_inner),
        [id],
        "the account's id, once"
    );
    assert!(
        stdout.contains("the running gatewayhub reconnected okx on the new credentials"),
        "{stdout}"
    );
    assert!(!stdout.contains(VALUE) && !stderr.contains(VALUE), "a VALUE reached a stream");
}

#[test]
fn with_no_datahub_answering_the_verb_says_the_next_start_and_exits_clean() {
    let (_root, settings, id) = project("127.0.0.1:1");
    let (code, stdout, stderr) = set_api_key(&settings, id);
    assert_eq!(
        code,
        Some(0),
        "the write landed: --- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert!(stdout.contains("no datahub answered at 127.0.0.1:1"), "{stdout}");
    assert!(stdout.contains("take effect at the account's next start"), "{stdout}");
    assert!(!stdout.contains(VALUE) && !stderr.contains(VALUE), "a VALUE reached a stream");
}
