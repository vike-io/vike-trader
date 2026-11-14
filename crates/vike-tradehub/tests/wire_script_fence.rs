//! **A strategy `file` that arrives over the wire may name a strategy file inside the node's
//! `strategies/` folder and nothing else**: `crates/vike-tradehub/src/mount_factory.rs`'s
//! `wire_file_refusal`, through the REAL server, at the preview and at the send.
//!
//! Its own binary and ONE `#[test]`, because what it stands on is per PROCESS: the fence reads the
//! strategies folder of the project the process declared (`vike_boot::halt::declare_project_state_dir`,
//! one per process). That is also why it cannot join `tests/daemon.rs` (`crates/vike-backtest/CLAUDE.md`'s
//! "Test-binary consolidation").
//!
//! ⚠ A failing step RECORDS and the test goes on, so one run shows every hole; and a path that would
//! stall or fill the core (a device, a FIFO) is NEVER sent unless the preview refused it, so this
//! test cannot wedge the box it runs on when the fence is broken.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use vike_boot::halt::declare_project_state_dir;
use vike_core::{CoreConfig, CoreHandle, spawn_core};
use vike_execution::testing::RecordingClient;
use vike_execution::{Account, BalanceMode, Engine, MountSpec, RiskGate};
use vike_hub_protocol::auth::{HubKeys, Scope};
use vike_log::poll::wait_until;
use vike_model::RiskLimits;
use vike_tradehub::mount_factory::{MAX_STRATEGY_FILE_BYTES, resolve_spec, strategy_factory};
use vike_tradehub::server::control::{ControlLimits, ControlLimitsConfig, accept_command};
use vike_tradehub::{publish, server};
use vike_tradehub_client::auth;
use vike_tradehub_client::preview_command;
use vike_tradehub_client::proto::{NODE_PROTO_VERSION, Request, Response, read_frame, write_frame};
use vike_tradehub_client::wire::CommandWire;

// The strategy FILES this test mounts (no strategy is compiled into the daemon).
#[allow(dead_code, clippy::allow_attributes)]
#[path = "../src/test_strategy_files.rs"]
mod strategy_files;

const OBSERVE_KEY: &[u8] = b"observe-secret-key-for-fence-tests";
const CONTROL_KEY: &[u8] = b"control-secret-key-for-fence-tests";
/// The value of the credential-shaped file. A header error quotes the line it could not read, so
/// it is what an unfenced mount of a link to that file would print into `MOUNT REFUSED`,
/// `recent_events` and `node_snapshot`.
const VALUE: &str = "s3cr3tvalue";
const SCRIPT: &str = strategy_files::NOTHING;

fn engine() -> Engine<RecordingClient> {
    Engine::new(
        Account::new(1.0, "binance", None, BalanceMode::Delta),
        RiskGate::new(RiskLimits::new()),
        RecordingClient::default(),
        "binance",
        "BTCUSDT",
    )
}

fn mount(id: &str, file: &str) -> CommandWire {
    CommandWire::MountStrategy {
        venue: "binance".into(),
        account: None,
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        controller_id: Some(id.into()),
        file: Some(file.into()),
        name: None,
        rhai: None,
        params: serde_json::json!({}),
    }
}

/// What the fold's factory is handed for a mount of `file`.
fn spec(file: &str) -> MountSpec {
    MountSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some("direct".into()),
        file: Some(file.into()),
        name: None,
        rhai: None,
        params: vike_model::params::Params::new(),
        order_client_id_code: None,
    }
}

/// The real server over `handle`, as `tests/control_roundtrip.rs`'s `serve_core` starts it.
fn serve_core(handle: &CoreHandle) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(handle.snapshot_cell(), None);
    let commands = Some(handle.command_sink());
    std::thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            HubKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()),
            commands,
            ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    addr
}

/// One `Command` on a fresh authed control connection, and its answer.
fn send(addr: SocketAddr, cmd: CommandWire) -> Response {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(CONTROL_KEY, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Write } => {}
        other => panic!("expected AuthOk(Write), got {other:?}"),
    }
    write_frame(&mut stream, &Request::Command { command: cmd, reason: None }).expect("command");
    read_frame::<_, Response>(&mut stream).expect("answer")
}

/// The preview's verdict on a mount of `file`.
fn preview(addr: SocketAddr, file: &str) -> (bool, Option<String>) {
    let (accepted, reason, _notes) = preview_command(addr, CONTROL_KEY, &mount("fenced", file))
        .expect("the preview is answered");
    (accepted, reason)
}

#[test]
fn a_wire_mount_reaches_a_strategy_file_and_nothing_else() {
    let project = tempfile::tempdir().expect("project");
    let elsewhere = tempfile::tempdir().expect("a folder outside the project");
    let root = project.path();
    let state = root.join("settings").join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    declare_project_state_dir(Some(state)).expect("the first declaration of this process");
    let strategies = root.join(vike_paths::state_path::PROJECT_STRATEGIES_DIR);

    strategy_files::write_into(
        &strategies,
        &[("sma_cross.rhai", SCRIPT), ("trend/sma_cross.rhai", SCRIPT), ("CAPS.RHAI", SCRIPT)],
    );

    // A credential-shaped file OUTSIDE the folder, whose first lines read as a header block: a
    // header error quotes the line it could not read, so a mount of it echoes the value.
    let notes = root.join("notes.env");
    std::fs::write(&notes, format!("// ```toml\n// KEY={VALUE}\n// ```\n")).expect("credentials");
    let foreign = elsewhere.path().join("elsewhere.rhai");
    std::fs::write(&foreign, SCRIPT).expect("a strategy file outside the project");
    std::fs::create_dir(strategies.join("folder.rhai")).expect("a folder named like a file");
    let over_cap = usize::try_from(MAX_STRATEGY_FILE_BYTES).expect("the cap fits") + 1;
    std::fs::write(strategies.join("big.rhai"), " ".repeat(over_cap)).expect("a file over the cap");

    let config = CoreConfig {
        seed_cash: 1.0,
        strategy_factory: Some(strategy_factory(Some(strategies.clone()), false)),
        ..CoreConfig::default()
    };
    let handle = spawn_core(engine(), config);
    let cell = handle.snapshot_cell();
    let addr = serve_core(&handle);
    let mut red: Vec<String> = Vec::new();

    // ── Refused at the door — by the path rule (`..`, absolute, not `.rs`/`.rhai`), a folder, a file
    //    over the cap — and sent anyway, so a run on an unfenced node shows what reaches the core.
    let plain = [
        "../notes.env".to_string(),
        "trend/../../notes.env".to_string(),
        notes.display().to_string(),
        "notes.env".to_string(),
        "folder.rhai".to_string(),
        "big.rhai".to_string(),
    ];
    for file in &plain {
        let (accepted, reason) = preview(addr, file);
        if accepted {
            red.push(format!("{file}: the preview accepted it"));
        }
        match send(addr, mount("fenced", file)) {
            Response::Error(msg) => {
                if reason.as_deref() != Some(msg.as_str()) {
                    red.push(format!("{file}: preview said {reason:?}, the send said {msg:?}"));
                }
                if msg.contains(VALUE) {
                    red.push(format!("{file}: the refusal echoes the value: {msg}"));
                }
            }
            other => red.push(format!("{file}: the send answered {other:?}")),
        }
    }

    // ── A link INSIDE the folder to a file outside it (unix; the dev box makes no links).
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&notes, strategies.join("link.rhai")).expect("a link to notes");
        std::os::unix::fs::symlink(&foreign, strategies.join("foreign.rhai")).expect("a link out");
        std::os::unix::fs::symlink(
            elsewhere.path().join("absent.rhai"),
            strategies.join("gone.rhai"),
        )
        .expect("a dangling link out");

        // The echo the fence exists for is REAL: the factory, handed the linked file (the profile's
        // route takes no link fence), prints its value.
        match resolve_spec(&spec("link.rhai"), &strategies, false) {
            Err(e) if e.contains(VALUE) => {}
            Err(e) => {
                red.push(format!("the factory no longer echoes a linked credential file: {e}"))
            }
            Ok(_) => red.push("the factory compiled a linked credential file".to_string()),
        }
        for file in ["link.rhai", "foreign.rhai"] {
            let (accepted, reason) = preview(addr, file);
            if accepted {
                red.push(format!("{file}: the preview accepted a link out of the folder"));
            }
            match send(addr, mount("fenced", file)) {
                Response::Error(msg) => {
                    if reason.as_deref() != Some(msg.as_str()) {
                        red.push(format!("{file}: preview said {reason:?}, the send said {msg:?}"));
                    }
                    if msg.contains(VALUE) {
                        red.push(format!("{file}: the refusal echoes the value: {msg}"));
                    }
                }
                other => red.push(format!("{file}: the send answered {other:?}")),
            }
        }
        // No answer tells a caller whether the file a link points at exists.
        let (_, one) = preview(addr, "foreign.rhai");
        let (_, other) = preview(addr, "gone.rhai");
        let one = one.unwrap_or_default().replace("foreign.rhai", "F");
        let other = other.unwrap_or_default().replace("gone.rhai", "F");
        if one.is_empty() || one != other {
            red.push(format!(
                "a link to a file and a dangling link answer differently: {one:?} / {other:?}"
            ));
        }
    }
    let echoed = wait_until(Duration::from_secs(2), || {
        cell.load_full().recent_events.iter().any(|l| l.contains(VALUE))
    });
    if echoed {
        red.push(format!(
            "the value reached recent_events: {:?}",
            cell.load_full().recent_events.iter().filter(|l| l.contains(VALUE)).collect::<Vec<_>>()
        ));
    }

    // ── What would stall or fill the fold: NEVER sent unless the preview refused it.
    #[cfg(unix)]
    {
        let fifo = strategies.join("pipe.rhai");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status().expect("mkfifo runs");
        assert!(made.success(), "mkfifo failed");
        std::os::unix::fs::symlink("/dev/zero", strategies.join("zero.rhai"))
            .expect("a device link");
        let hostile = ["/dev/zero", "/proc/self/environ", "pipe.rhai", "zero.rhai"];
        for file in hostile {
            let (accepted, reason) = preview(addr, file);
            if accepted {
                red.push(format!("{file}: the preview accepted it"));
                continue;
            }
            match send(addr, mount("fenced", file)) {
                Response::Error(msg) if reason.as_deref() == Some(msg.as_str()) => {}
                other => red.push(format!("{file}: preview {reason:?}, send {other:?}")),
            }
        }
    }

    // ── No answer tells a caller whether a file outside the project exists.
    for (present, absent) in
        [("/etc/passwd", "/etc/no-such-file-here"), ("../../notes.env", "../../no-such-file.rhai")]
    {
        let (_, one) = preview(addr, present);
        let (_, other) = preview(addr, absent);
        let one = one.unwrap_or_default().replace(present, "P");
        let other = other.unwrap_or_default().replace(absent, "P");
        if one.is_empty() || one != other {
            red.push(format!("{present} and {absent} answer differently: {one:?} / {other:?}"));
        }
    }

    // ── The read happens at the door, not on the fold: a core with no factory never reads, and
    //    the file is gone by the time the factory is asked, yet the source is there.
    let idle = spawn_core(engine(), CoreConfig { seed_cash: 1.0, ..CoreConfig::default() });
    strategy_files::write_into(&strategies, &[("door.rhai", SCRIPT)]);
    let mut limits = ControlLimits::new(ControlLimitsConfig::default());
    let accepted = accept_command(
        mount("door", "door.rhai"),
        None,
        &mut limits,
        &idle.command_sink(),
        None,
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        &[],
        None,
        None,
        None,
    );
    if let Err(e) = accepted {
        red.push(format!("the door refused a strategy file inside the folder: {e}"));
    }
    std::fs::remove_file(strategies.join("door.rhai")).expect("remove the file");
    if let Err(e) = resolve_spec(&spec("door.rhai"), &strategies, false) {
        red.push(format!("the factory went to the file for a wire mount: {e}"));
    }
    idle.shutdown_and_join();

    // ── What an operator mounts still mounts: a file at the folder's top, one in a subfolder, and
    //    one saved under a capitalised extension.
    for (id, file) in
        [("top", "sma_cross.rhai"), ("nested", "trend/sma_cross.rhai"), ("caps", "CAPS.RHAI")]
    {
        let (accepted, reason) = preview(addr, file);
        if !accepted {
            red.push(format!("{file}: the preview refused it: {reason:?}"));
        }
        if !matches!(send(addr, mount(id, file)), Response::Ack { .. }) {
            red.push(format!("{file}: the send was not acked"));
        }
        let landed = wait_until(Duration::from_secs(5), || {
            cell.load_full().mounts.iter().any(|m| m.mount_id == id)
        });
        if !landed {
            red.push(format!("{file}: never mounted: {:?}", cell.load_full().recent_events));
        }
    }

    handle.shutdown_and_join();
    assert!(red.is_empty(), "{}", red.join("\n"));
}
