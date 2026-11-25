// Compiled into ONE test binary, the `shipped_cli` group (`tests/shipped_cli/main.rs`); a member that
// stops calling an item should not have to delete it from the shared kit, hence the allow.
#![allow(dead_code)]
//! Shared test-support for the `vike-cli` cases that drive the SHIPPED binary: the plain runners
//! ([`run`], [`run_in`]), the scratch credential store ([`seed_store`]) and this box's device key
//! ([`seed_device`]), and the positive-argv pin
//! harness ([`accepted`]). A case that waits for another thread polls through
//! `vike_log::poll::wait_until`.
//!
//! The planted-engine plant and its `ETXTBSY` retry are NOT here: they are
//! `crates/vike-cli/src/test_support.rs` (`vike_cli::test_support`, behind the `test-support`
//! feature), compiled once by the library because `tests/planted_binary_retry.rs` needs them too.

use std::process::{Command, Output};

// ------------------------------------------------------------------------------------------------
// The plain runners: cases that plant nothing
// ------------------------------------------------------------------------------------------------
//
// ⚠ These do NOT retry `ETXTBSY`: nothing they run was planted by the case, so there is no write
// descriptor to race. A case that plants an engine keeps using its file's own runner, which goes
// through `vike_cli::test_support::output_retrying_etxtbsy`.

/// Run the shipped binary against a FRESH, EMPTY settings directory, with `envs` layered on top of
/// the risk-variable and hub-key clears every caller relies on.
///
/// The two removed-variable removals are not decoration: `vike_config::refuse_removed_env` runs
/// before any verb is routed, so an exported `VIKE_MAX_ORDER_NOTIONAL` on the developer's box
/// would make every case exit on the startup-refusal path instead of the rung it is testing.
pub fn run(args: &[&str], envs: &[(&str, &str)]) -> Output {
    let empty = tempfile::tempdir().expect("tempdir");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", empty.path())
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

/// [`run`]'s twin for the cases that need a POPULATED project: `VIKE_SETTINGS_DIR` is `settings`
/// and `VIKE_USER_DATA_DIR` is `user_data`, keeping every `env_remove`. A second helper rather
/// than an edit, because the emptiness of [`run`]'s directory is what its callers rely on.
pub fn run_in(settings: &std::path::Path, user_data: &std::path::Path, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_vike-cli"));
    cmd.args(args)
        .env("VIKE_SETTINGS_DIR", settings)
        .env("VIKE_USER_DATA_DIR", user_data)
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY");
    cmd.output().unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

// ---------------------------------------------------------------------------------------------
// The credential store a scratch project needs
// ---------------------------------------------------------------------------------------------

/// **Create a scratch settings directory's settings DATABASE and write `rows` into it** — the one
/// credential store since the credential FILE store was removed (2026-10-07). The store is created
/// the way a fresh box creates it (`vike_secrets::create_store`
///, the library half of `vike-cli secrets init`),
/// so an empty `rows` still leaves the
/// (empty) store a fixture asked for. Each row lands where the production writer puts it (through
/// its record-less body, `vike_secrets::save_credentials_to_store_unjournalled`: a seed is not an
/// operator act, so no ledger line is written): a
/// platform key (`vike_venues::credential_keys::is_platform_key`, the daemons' keys) in the
/// `hub_key` table, every other name in the `credential` table as a DEPLOYMENT row — no account,
/// no venue: a name-addressed write creates no account (decision 0156 item 6). The database is
/// created 0600 in a 0700 directory, so no permission finding buries a case's own transcript.
/// A box that dials a hub holds a DEVICE key instead ([`seed_device`]).
pub fn seed_store(settings: &std::path::Path, rows: &[(&str, &str)]) {
    vike_secrets::create_store(Some(settings.to_str().expect("a UTF-8 scratch path")))
        .expect("create the scratch settings database");
    let mut node = Vec::new();
    let mut credential = Vec::new();
    for (k, v) in rows {
        let row = (k.to_string(), v.to_string());
        if vike_venues::credential_keys::is_platform_key(k) {
            node.push(row);
        } else {
            credential.push(row);
        }
    }
    if !node.is_empty() {
        vike_secrets::save_credentials_to_store_unjournalled(
            settings,
            vike_secrets::StoreTable::HubKey,
            &node,
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("write the fixture's hub keys");
    }
    if !credential.is_empty() {
        vike_secrets::save_credentials_to_store_unjournalled(
            settings,
            vike_secrets::StoreTable::Credential,
            &credential,
            vike_secrets::CredentialUpsertIntent::Add,
        )
        .expect("write the fixture's credentials");
    }
}

/// **Give `settings`' box its DEVICE key** (decision 0288 item 8: the shipped binaries sign with this box's
/// `device_key` row and nothing else), creating the store if it is absent. The fixture VALUE names the device
/// ([`device_for`]), so a node keyed with the same value ([`fixture_access`]) lists it at the role given there, and
/// any other value ("not-a-real-key") is a device that node does not list.
pub fn seed_device(settings: &std::path::Path, value: &str) {
    seed_store(settings, &[]);
    let (device, pkcs8) = device_for(value);
    vike_secrets::access::device_key_insert(settings, &device.key_id(), "owner", "cli", &pkcs8)
        .expect("write the fixture's device key");
}

/// **The fixtures' device keys**: ONE Ed25519 pair per key VALUE a fixture spells, made once per test process, so a
/// store seeded with a value ([`seed_device`]) and a node keyed with the same value ([`fixture_access`]) agree, and a
/// store seeded with another value ("not-a-real-key") holds a device that node does not list.
pub fn device_for(value: &str) -> (vike_hub_protocol::access::DeviceKey, Vec<u8>) {
    /// One fixture value's pair: the key and its PKCS#8 document.
    type Made = (vike_hub_protocol::access::DeviceKey, Vec<u8>);
    static MADE: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, Made>>> =
        std::sync::OnceLock::new();
    let made = MADE.get_or_init(Default::default);
    let mut made = made.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    made.entry(value.to_string())
        .or_insert_with(|| vike_hub_protocol::access::DeviceKey::generate().expect("a device key"))
        .clone()
}

/// The access list of a fixture node keyed with `observe` / `control` VALUES: the device of `observe` at the role
/// `observe` and the device of `control` at the role `control` (an empty value lists nothing).
pub fn fixture_access(observe: &str, control: &str) -> vike_hub_protocol::access::AccessList {
    fixture_access_at(&[(observe, "observe"), (control, "control")])
}

/// [`fixture_access`] with each VALUE's role named: `(value, role)` lists the device of `value` at `role`.
pub fn fixture_access_at(listed: &[(&str, &str)]) -> vike_hub_protocol::access::AccessList {
    let mut rows = std::collections::HashMap::new();
    for &(value, role) in listed {
        if value.is_empty() {
            continue;
        }
        let (device, _) = device_for(value);
        rows.insert(
            device.key_id(),
            vike_hub_protocol::access::AccessEntry {
                person: "owner".into(),
                device: "cli".into(),
                role: role.into(),
                public_key: device.public_key().to_vec(),
                revoked: false,
            },
        );
    }
    vike_hub_protocol::access::AccessList::new(move |key_id| rows.get(key_id).cloned())
}

// ---------------------------------------------------------------------------------------------
// A REAL paper node and the store that addresses it
// ---------------------------------------------------------------------------------------------

/// One resting order of `venue`, its `account` as the wire would carry it.
pub fn wire_order(
    order_client_id: &str,
    venue: &str,
    account: Option<&str>,
) -> vike_tradehub_client::wire::OrderViewWire {
    serde_json::from_value(serde_json::json!({
        "order_client_id": order_client_id, "venue": venue, "account": account, "symbol": "BTCUSDT",
        "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0, "trigger_price": null,
        "status": "ACCEPTED", "venue_order_id": null, "filled_qty": 0.0, "average_fill_price": 0.0,
    }))
    .expect("an order")
}

/// A scripted node for an OBSERVE read: the real handshake (the mac accepted unseen), then `frame`
/// pushed on `Subscribe`, the socket held until the client hangs up. It serves connections one after
/// another for as long as the case runs, for the states a fresh paper core never reaches.
pub fn spawn_frame_node(
    frame: vike_tradehub_client::wire::CoreSnapshotWire,
) -> std::net::SocketAddr {
    use vike_hub_protocol::frame::{read_frame, write_frame};
    use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};
    use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve the assigned port");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let Ok(HandshakeFrame::Hello { .. }) = read_frame::<_, HandshakeFrame>(&mut stream)
            else {
                continue;
            };
            let welcome = HandshakeReply::Welcome {
                proto_version: NODE_PROTO_VERSION,
                features: vec![vike_hub_protocol::access::FEATURE_PERSON_AUTH.to_string()],
                nonce: Some([7u8; 32]),
            };
            let _ = write_frame(&mut stream, &welcome);
            let Ok(HandshakeFrame::Auth { scope, .. } | HandshakeFrame::PersonAuth { scope, .. }) =
                read_frame::<_, HandshakeFrame>(&mut stream)
            else {
                continue;
            };
            let _ = write_frame(&mut stream, &HandshakeReply::AuthOk { scope, admitted: None });
            let Ok(Request::Subscribe { .. }) = read_frame::<_, Request>(&mut stream) else {
                continue;
            };
            let _ = write_frame(&mut stream, &Response::SnapshotFrame(Box::new(frame.clone())));
            while read_frame::<_, Request>(&mut stream).is_ok() {}
        }
    });
    addr
}

/// Far-future resolution so the A-S horizon is positive.
const PAPER_NODE_RESOLUTION_TIMESTAMP: i64 = 3_000_000_000;

/// A PAPER `vike-tradehub` node mounted on `venue`/`token` and served on an ephemeral loopback port
/// with CONTROL enabled, listing the devices of `observe_key`/`control_key` at those roles
/// ([`fixture_access`]; the shared bytes beside them are a test's, no shipped client sends one): the smallest fixture that can
/// genuinely REFUSE (or accept) a command server-side. Hold the returned mount for the case's
/// lifetime. `tests/shipped_cli/exit_codes.rs`'s `venue_reject_fixture` and
/// `tests/shipped_cli/trade_plane_cli.rs`'s `json_read_fixture` name their own venue, token and
/// key values over it.
pub fn spawn_paper_node(
    venue: &str,
    token: &str,
    observe_key: &str,
    control_key: &str,
) -> (vike_tradehub_core::StrategyCorePaper, std::net::SocketAddr) {
    use vike_tradehub::{publish, server};

    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategy_lab::makers::spread_maker::SpreadMaker::from_params,
        venue,
        token,
        Some(PAPER_NODE_RESOLUTION_TIMESTAMP),
    );
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let keys = vike_hub_protocol::auth::HubKeys::new(
        observe_key.as_bytes().to_vec(),
        control_key.as_bytes().to_vec(),
    )
    .with_access(fixture_access(observe_key, control_key));
    std::thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    (mount, addr)
}

/// A throwaway `<project>/settings` whose settings database holds the device key of `value` ([`seed_device`]),
/// so `vike-cli`'s dispatcher signs with it exactly as it would on a real box.
pub fn device_settings_dir(value: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    seed_device(dir.path(), value);
    dir
}

// ---------------------------------------------------------------------------------------------
// The positive-argv pin harness (decision 0155)
// ---------------------------------------------------------------------------------------------

pub mod accepted;
