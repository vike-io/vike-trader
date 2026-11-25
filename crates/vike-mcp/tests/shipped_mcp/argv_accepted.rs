//! The command lines an operator, a runbook or a scheduled run writes, pinned as ACCEPTED over the
//! shipped binary (decision 0155). stdin is closed, so a started server answers EOF with exit 0; a
//! hang is killed with no code, which fails the pin. The exact answers to a line that is refused are
//! the goldens' (`crates/vike-mcp/tests/shipped_mcp/mcp_wire_snapshot/process.rs`).

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How long one command line may run before it is killed.
const LIMIT: Duration = Duration::from_secs(30);

const THE_THREE_PLANES: &[&str] =
    &["--addr", "127.0.0.1:7878", "--backtest-addr", "127.0.0.1:7880", "--node", "127.0.0.1:7879"];

const ACCEPTED: &[(&[&str], &str)] = &[
    (&[], "README.md: the bare server returns at EOF"),
    (&["--addr", "127.0.0.1:7878"], "--addr"),
    (&["--addr=127.0.0.1:7878"], "inline --addr"),
    (&["--backtest-addr", "127.0.0.1:7880"], "--backtest-addr"),
    (&["--backtest-addr=127.0.0.1:7880"], "inline --backtest-addr"),
    (&["--node", "127.0.0.1:7879"], "the connect-to-a-node skill"),
    (&["--node=127.0.0.1:7879"], "inline --node"),
    (THE_THREE_PLANES, "docs/ops: all three planes"),
    (
        &["--addr", "127.0.0.1:7878", "--node", "127.0.0.1:7879"],
        "connect-to-a-node.md: [--addr ..] [--node <host:port>]",
    ),
    (&["--profile", "full"], "scope.rs: PROFILES"),
    (&["--profile", "read-only"], "scope.rs: PROFILES"),
    (&["--profile", "offline"], "scope.rs: PROFILES"),
    (&["--profile=read-only"], "inline --profile"),
    (&["--deny-tool", "submit_order"], "config.rs: a served tool name"),
    (
        &["--deny-tool", "submit_order", "--deny-tool", "delete_series", "--deny-tool=set_setting"],
        "a repeatable flag, both spellings",
    ),
    (&["--trace"], "docs/ops: into <project>/settings/state/agent/"),
    (&["--trace-dir", "agent-log"], "docs/ops: --trace-dir <dir> (relative here)"),
    (&["--trace-dir=agent-log"], "inline --trace-dir"),
    (&["--trace", "--trace-dir", "agent-log"], "two spellings of one destination"),
    (&["--trace-dir", "agent-log", "--trace"], "two spellings of one destination, reversed"),
    (&["--unattended"], "decision 0040: the unattended runner's flag"),
    (
        &[
            "--profile",
            "read-only",
            "--trace-dir",
            "agent-record",
            "--unattended",
            "--node",
            "127.0.0.1:7879",
            "--addr",
            "127.0.0.1:7878",
        ],
        "vike-agent-evaluation unattended.rs mcp_argv: the exact scheduled-run argv",
    ),
    (
        &[
            "--profile",
            "offline",
            "--deny-tool",
            "list_series",
            "--trace",
            "--unattended",
            "--addr",
            "127.0.0.1:7878",
            "--backtest-addr",
            "127.0.0.1:7880",
            "--node",
            "127.0.0.1:7879",
        ],
        "every flag at once",
    ),
];

/// A single-value flag given twice: `clap` refuses it rather than reading one of them.
const REFUSED: &[(&[&str], &str)] = &[
    (&["--addr", "127.0.0.1:1", "--addr", "127.0.0.1:7878"], "a repeated --addr"),
    (&["--profile", "full", "--profile", "read-only"], "a repeated --profile"),
];

/// The hubs' well-known loopback addresses, as the documented lines above WRITE them. the CI box runs the
/// PRODUCTION hubs there beside the test lanes, so [`run`] swaps each for a port it bound and closed
/// (refused at once) before the spawn: the reader is handed the same `host:port` shape, which is all
/// it checks, and a server that dialled at start would reach nothing.
const HUB_ADDRESSES: [&str; 3] = ["127.0.0.1:7878", "127.0.0.1:7879", "127.0.0.1:7880"];

/// The exit code of the shipped binary on `argv`, in a fresh project, and its stderr.
fn run(argv: &[&str]) -> (Option<i32>, String) {
    let sandbox = tempfile::tempdir().expect("a scratch directory");
    let settings = sandbox.path().join("settings");
    std::fs::create_dir_all(&settings).expect("a scratch settings directory");
    let dead = std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .expect("a free loopback port")
        .to_string();
    let swapped: Vec<String> = argv
        .iter()
        .map(|token| HUB_ADDRESSES.iter().fold(token.to_string(), |t, hub| t.replace(hub, &dead)))
        .collect();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-mcp"))
        .args(&swapped)
        .current_dir(sandbox.path())
        .env_clear()
        .env("VIKE_SETTINGS_DIR", &settings)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap_or_else(|e| panic!("spawn vike-mcp {argv:?}: {e}"));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = child.kill();
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("wait for vike-mcp {argv:?}: {e}"),
        }
    }
    let out = child.wait_with_output().unwrap_or_else(|e| panic!("collect vike-mcp {argv:?}: {e}"));
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn the_command_lines_people_write_are_read_as_written() {
    let mut failures = Vec::new();
    for (lines, want) in [(ACCEPTED, 0), (REFUSED, 2)] {
        for (argv, why) in lines {
            let (code, stderr) = run(argv);
            if code != Some(want) {
                failures.push(format!(
                    "  vike-mcp {} ({why})\n    expected {want}, got {code:?}\n    stderr: {}",
                    argv.join(" "),
                    stderr.trim()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "the reader moved:\n{}", failures.join("\n"));
}
