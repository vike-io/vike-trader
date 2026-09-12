//! End-to-end proof that `vike-cli trade status` actually REACHES the wire verbs — the gap the
//! I10 rehearsal found: `vike_tradehub_client::strategy_status` (split-plane B4) existed, and no
//! CLI called it, so asking a running node "what are you running" meant compiling a throwaway
//! probe (`docs/ops/i10-rehearsal-2026-08-19.md`).
//!
//! ⚠ The command was `vike-cli strategy-status` until ruling 17 of
//! `docs/superpowers/specs/2026-09-09-datahub-market-data-wire-design.md`; it is `trade status`
//! now, and it answers with the trading MODE as well as the registry — so this file drives two
//! wire reads (`Request::StrategyStatus` and `Request::Snapshot`) where it used to drive one.
//!
//! Hermetic and loopback-only, the `tests/trade_node_e2e.rs` shape: a REAL paper node
//! (`vike_run::build_paper_maker_core` + the real `vike_tradehub::{server, publish}`) on an
//! ephemeral `127.0.0.1:0` port, driven by the SHIPPED bin via `CARGO_BIN_EXE_vike-cli`. No
//! credentials, no venue, no external network; the observe key is an obviously-fake `DUMMY-*`
//! string written into a throwaway settings directory — deliberately the STORE and not the
//! environment, so every run re-proves the keyring resolves the store the daemon itself reads.
//!
//! # What each test would have caught
//!
//! - [`trade_status_renders_the_mode_the_node_identity_and_mount_rows`] — the whole feature: the
//!   shipped binary, the observe handshake, BOTH wire reads, and a human output carrying the
//!   trading mode above a table with the daemon identity line plus one row per mount (an I10
//!   multi-mount node, so BOTH rows must appear — a client reading only the identity block would
//!   print one).
//! - [`trade_status_json_nests_the_wire_payload_beside_the_mode`] — the machine surface: `--json`
//!   parses, the registry half carries the wire's own field names under `strategy_status` so a
//!   script written against the node's schema works unchanged, and the mode rides beside it.
//! - [`an_identity_less_node_is_an_honest_error`] — the server-error path: a node publishing no
//!   identity block answers `Response::Error`, and the CLI must exit 1 carrying the server's own
//!   words, never a fabricated empty table — while still printing the MODE, which that same node
//!   answers perfectly well.
//! - [`an_old_node_is_refused_client_side_but_still_answers_the_mode`] — the feature-negotiation
//!   refusal, against a SCRIPTED node whose `Welcome.features` lacks `strategy-verbs` (the real
//!   server always advertises it, so an "old node" must be scripted). Three halves, all essential:
//!   the CLI's message is actionable (names the capability, says to upgrade the node); the scripted
//!   node OBSERVES that no frame follows the handshake on that connection — sending one would hand
//!   an old server an undecodable frame and the operator an opaque decode error instead of a
//!   diagnosis; and the MODE is asked for on a second connection and printed, because
//!   `Request::Snapshot` is a base verb an old node still serves and "is this node halted" is the
//!   half you cannot leave unanswered.
//! - [`a_failed_mode_read_is_a_json_key_and_a_stderr_line`] — the other direction, against a
//!   scripted node that answers the registry and refuses the snapshot: a failed read is a
//!   `trading_state_error` key AND a stderr line, never an omission.
//! - [`with_no_observe_key_anywhere_the_message_names_both_sources`] — the honest failure: exit 1
//!   with a message naming the observe key, both places it was looked for, and the store path.
//! - [`legacy_keys_still_authenticate_and_say_so`] — the MIGRATION, which every other test in this
//!   file stopped exercising when the fixture moved to `node.env`: a box whose pair is still in
//!   `secrets.env` must keep authenticating (a deploy that took a running node's auth away is the
//!   failure the fallback exists to prevent) AND must be told where the pair belongs now, or the
//!   fallback is silent, nothing migrates, and the read can never be deleted.

use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::sync::mpsc;
use std::thread;

use vike_run::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::wire::{
    WireMountRow, WireNodeIdentity, WireSnapshot, WireStrategyStatus, WireTradingState,
};
use vike_tradehub_client::{
    NODE_PROTO_VERSION, NodeKeys, Request, Response, Scope, read_frame, write_frame,
};

/// The paper mount's symbol. Any string works — nothing resolves it against a venue.
const TOKEN: &str = "CLI_TRADE_STATUS_E2E_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the trade e2e's mount).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// Obviously fake: an HMAC secret, so any bytes work as long as both sides agree — and the
/// `DUMMY-` prefix makes it unmistakable in a diff or a process listing that no real credential
/// is involved.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-trade-status-e2e";

/// The identity block the node under test publishes — what the daemon builds from its profile.
fn identity() -> WireNodeIdentity {
    WireNodeIdentity {
        name: "e2e-hub".to_string(),
        strategy: "spread_maker".to_string(),
        params: "spread=0.01".to_string(),
        live: false,
        build: "vike-tradehub test-build".to_string(),
    }
}

/// Two mount rows — an I10 `[[mounts]]` daemon, so the table must carry BOTH.
fn mounts() -> Vec<WireMountRow> {
    vec![
        // The addressing key + typed params a node advertising `strategy-params` fills in; these
        // fixtures keep them EMPTY, which is what a node predating that capability sends — so the
        // table and `--json` assertions below are unchanged by the fields existing.
        WireMountRow {
            strategy: "spread_maker".to_string(),
            params: "venue=polymarket symbol=TOK-A spread=0.01".to_string(),
            live: false,
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            typed_params: None,
        },
        WireMountRow {
            strategy: "np".to_string(),
            params: "venue=polymarket symbol=TOK-B edge=0.02".to_string(),
            live: false,
            venue: String::new(),
            symbol: String::new(),
            interval: String::new(),
            typed_params: None,
        },
    ]
}

/// Build a PAPER node and serve it on an ephemeral loopback port, observe scope only (no control
/// key, no command sink — this suite drives a READ verb). The returned mount keeps the core alive.
fn spawn_node(
    identity: Option<WireNodeIdentity>,
    mounts: Vec<WireMountRow>,
) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn_with_mounts(mount.handle.snapshot_cell(), identity, mounts);
    let keys = NodeKeys::new(OBSERVE_KEY.as_bytes().to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            None,
            // No REQ-2 datahub advertisement — this suite exercises the `trade status` read path only.
            None,
        );
    });
    (mount, addr)
}

/// A throwaway `<project>/settings` directory holding a NODE-KEY store with (only) the observe
/// key — the daemon's own documented configuration, and the resolution path under test.
///
/// ⚠ This planted `secrets.env` until 2026-09-08, and repointing it is not bookkeeping: with the
/// keys in the old file every run takes the MIGRATION fallback, which prints a notice on stderr —
/// so the happy path's "nothing lands on stderr" assertion was measuring a box that had not
/// migrated. [`legacy_keys_still_authenticate_and_say_so`] is what covers that box now, deliberately
/// and once.
fn settings_dir_with_store(keys: &[(&str, &str)]) -> tempfile::TempDir {
    settings_dir_with_store_in(vike_secrets::NODE_FILE, keys)
}

/// The same fixture with the FILE as a parameter, so one test can plant the legacy store.
fn settings_dir_with_store_in(file: &str, keys: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut body = String::new();
    for (k, v) in keys {
        body.push_str(&format!("{k}={v}\n"));
    }
    let path = dir.path().join(file);
    std::fs::write(&path, body).expect("write store");
    // 0600 like a real store, so the permission finding does not bury the assertions' transcripts.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    }
    dir
}

/// What one `vike-cli trade status` run produced.
struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
    /// The exit RUNG (`crates/vike-cli/src/exit.rs`). Kept beside `ok` rather than derived from it
    /// because the distinction this verb has to get right is not success-vs-failure but WHICH
    /// failure: a node that answered and refused is a `1`, and only an unreachable socket is a `3`.
    code: Option<i32>,
}

/// Run the SHIPPED bin against `addr`. The node-key variables are explicitly REMOVED from the
/// child's environment: the configuration under test is "key in the store, nothing exported", and
/// a developer box exporting them would otherwise pass these vacuously.
fn run_cli(addr: &str, settings_dir: &std::path::Path, extra: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["trade", "status", "--node", addr])
        .args(extra)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        // The two REMOVED risk variables refuse startup when set — cleared so an operator box's
        // stale export cannot fail an unrelated read verb's test.
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-cli trade status");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        ok: out.status.success(),
        code: out.status.code(),
    }
}

/// The whole feature, through the shipped binary: the trading MODE + identity line + BOTH mount
/// rows, in one output.
///
/// ⚠ The mode assertion is the half ruling 17 added, and it is what proves the SECOND wire read
/// happened. Without it this test passes on a build that only ever sent `Request::StrategyStatus` —
/// which is exactly what the command did before the ruling, and exactly what a regression would
/// look like: a `status` that quietly stops answering "is this node halted".
#[test]
fn trade_status_renders_the_mode_the_node_identity_and_mount_rows() {
    let (_mount, addr) = spawn_node(Some(identity()), mounts());
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "expected success:\n{transcript}");

    // The MODE, read off a point-in-time snapshot: a fresh paper core is Active.
    assert!(run.stdout.contains("trading state: Active"), "{transcript}");
    // The daemon identity line: name, mode, build.
    assert!(
        run.stdout.contains("node e2e-hub — paper — build vike-tradehub test-build"),
        "{transcript}"
    );
    assert!(run.stdout.contains("params: spread=0.01"), "{transcript}");
    // One row per mount — both of them, with their own params.
    assert!(run.stdout.contains("spread_maker"), "{transcript}");
    assert!(run.stdout.contains("venue=polymarket symbol=TOK-A spread=0.01"), "{transcript}");
    assert!(run.stdout.contains("venue=polymarket symbol=TOK-B edge=0.02"), "{transcript}");
    // Read-only surface: nothing lands on stderr on the happy path.
    assert!(run.stderr.trim().is_empty(), "{transcript}");
}

/// `--json` nests the WIRE payload beside the mode: it parses, the registry half carries the
/// wire's own field names under `strategy_status`, and `trading_state` rides at the top level.
#[test]
fn trade_status_json_nests_the_wire_payload_beside_the_mode() {
    let (_mount, addr) = spawn_node(Some(identity()), mounts());
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &["--json"]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "expected success:\n{transcript}");

    let v: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{transcript}"));
    assert_eq!(v["trading_state"], "Active", "{transcript}");
    let s = &v["strategy_status"];
    assert_eq!(s["identity"]["name"], "e2e-hub", "{transcript}");
    assert_eq!(s["identity"]["live"], false, "{transcript}");
    assert_eq!(s["effective_params"], "spread=0.01", "{transcript}");
    let rows = s["mounts"].as_array().expect("mounts is an array");
    assert_eq!(rows.len(), 2, "{transcript}");
    assert_eq!(rows[1]["strategy"], "np", "{transcript}");
}

/// A node publishing no identity block answers `Response::Error`; the CLI exits 1 carrying the
/// server's own words — never a fabricated empty table.
///
/// ⚠ The RUNG is the load-bearing half, and it is asserted rather than implied. This node
/// CONNECTED and passed the observe handshake; the refusal came back over the wire as
/// `io::ErrorKind::InvalidData`, and a catch-all that read that as a connect failure would answer
/// `3` — telling a retry wrapper to back off and try again forever against a permanent
/// configuration fact. `crates/vike-cli/src/cmd/trade_status.rs`'s `failure_exit` carries the
/// rule this pins ("the node ANSWERED").
#[test]
fn an_identity_less_node_is_an_honest_error() {
    let (_mount, addr) = spawn_node(None, Vec::new());
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "must fail:\n{transcript}");
    assert!(run.stderr.contains("publishes no identity block"), "{transcript}");
    assert_eq!(run.code, Some(1), "a node that ANSWERED is never the connect rung:\n{transcript}");
    // …and the MODE still comes back. This node answers `Request::Snapshot` perfectly well; the
    // half it cannot answer is the registry, and only that half may go missing.
    assert!(
        run.stdout.contains("trading state: Active"),
        "a registry failure may not take the kill-switch reading with it\n{transcript}"
    );
}

/// What one CONNECTION to a scripted node carried, post-handshake: the request kinds it received,
/// in order. An EMPTY vec is the "nothing was sent" observation the old-node contract turns on.
type ConnLog = Vec<String>;

/// A SCRIPTED node, serving `connections` short-lived connections in order and recording what each
/// one carried.
///
/// The real server cannot be configured to LACK a capability it serves (it always advertises what
/// it has), and it cannot be made to refuse a `Request::Snapshot` it can answer — so both of the
/// degrade paths below need a double. `features` is what its `Welcome` advertises; `answer` decides
/// what each post-auth request gets back, and `None` means "read the next frame instead of
/// replying", which is how a connection that receives nothing terminates.
///
/// ⚠ `vike-cli trade status` opens ONE connection per wire read, in a fixed order — the registry
/// first, then the mode — so a test reads the connection log positionally and does not have to
/// correlate anything.
///
/// The channel carries one [`ConnLog`] per connection, or `Err` with the step that desynced.
fn spawn_scripted_node(
    features: Vec<String>,
    connections: usize,
    answer: impl Fn(&Request) -> Option<Response> + Send + 'static,
) -> (SocketAddr, mpsc::Receiver<Result<Vec<ConnLog>, String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let verdict = (|| -> Result<Vec<ConnLog>, String> {
            let mut logs = Vec::new();
            for _ in 0..connections {
                let (mut stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
                match read_frame::<_, Request>(&mut stream) {
                    Ok(Request::Hello { .. }) => {}
                    other => return Err(format!("expected Hello, got {other:?}")),
                }
                write_frame(
                    &mut stream,
                    &Response::Welcome {
                        proto_version: NODE_PROTO_VERSION,
                        nonce: [7u8; 32],
                        features: features.clone(),
                    },
                )
                .map_err(|e| format!("write Welcome: {e}"))?;
                match read_frame::<_, Request>(&mut stream) {
                    Ok(Request::Auth { .. }) => {}
                    other => return Err(format!("expected Auth, got {other:?}")),
                }
                // A scripted double: accept the mac unseen — the client under test signs honestly,
                // and what this node exists to observe is what comes AFTER the auth.
                write_frame(&mut stream, &Response::AuthOk { scope: Scope::Observe })
                    .map_err(|e| format!("write AuthOk: {e}"))?;
                // Then read until the client closes, logging every frame. A connection the client
                // refused CLIENT-side logs nothing at all, which is the contract the old-node test
                // asserts.
                let mut log = ConnLog::new();
                while let Ok(req) = read_frame::<_, Request>(&mut stream) {
                    log.push(format!("{req:?}"));
                    if let Some(resp) = answer(&req) {
                        write_frame(&mut stream, &resp)
                            .map_err(|e| format!("write response: {e}"))?;
                    }
                }
                logs.push(log);
            }
            Ok(logs)
        })();
        let _ = tx.send(verdict);
    });
    (addr, rx)
}

/// Collect the scripted node's verdict, or fail with the step that desynced.
fn scripted_verdict(
    rx: &mpsc::Receiver<Result<Vec<ConnLog>, String>>,
    transcript: &str,
) -> Vec<ConnLog> {
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .unwrap_or_else(|e| panic!("the scripted node never reported ({e}):\n{transcript}"))
        .unwrap_or_else(|e| panic!("the scripted node desynced: {e}\n{transcript}"))
}

/// The registry payload a scripted node answers `Request::StrategyStatus` with — the same identity
/// and mount rows the REAL node in this file publishes, so a test can vary the SNAPSHOT half
/// without also varying this one.
fn scripted_registry() -> WireStrategyStatus {
    WireStrategyStatus {
        identity: identity(),
        effective_params: "spread=0.01".to_string(),
        mounts: mounts(),
    }
}

/// A snapshot carrying one fact — the trading MODE — for a scripted node to answer with.
fn scripted_snapshot(state: WireTradingState) -> WireSnapshot {
    let mut snap = WireSnapshot::empty();
    snap.seq = 1;
    snap.trading_state = state;
    snap
}

/// **The old-node degrade, and it is THREE properties in one run.**
///
/// A node advertising no `strategy-verbs` (what EVERY node looked like before B4 shipped):
///
/// 1. the registry verb is refused CLIENT-side with an actionable message, and the scripted node
///    observes that NOTHING was sent on that connection — an old server's serde cannot decode the
///    frame, so sending one would hand the operator an opaque decode error instead of a diagnosis;
/// 2. **the trading MODE is asked for ANYWAY, on a second connection, and printed.**
///    `Request::Snapshot` is a base verb of the protocol, so a node too old for the registry can
///    still answer the half that says whether it is halted. Until 2026-09-10 this command returned
///    before that read: `trade status` against an old node printed NO MODE AT ALL, which is the
///    silence the whole ruling exists to remove;
/// 3. the exit stays the registry's — a partial answer must not read as a whole one.
#[test]
fn an_old_node_is_refused_client_side_but_still_answers_the_mode() {
    // Pre-B4 features, and a node that answers `Request::Snapshot` as every node always has.
    let (addr, rx) = spawn_scripted_node(vec!["subscribe".to_string()], 2, |req| match req {
        Request::Snapshot => {
            Some(Response::SnapshotFrame(Box::new(scripted_snapshot(WireTradingState::Halted))))
        }
        _ => None,
    });
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "must fail: the registry half genuinely failed:\n{transcript}");
    assert_eq!(run.code, Some(1), "a node that ANSWERED is never the connect rung:\n{transcript}");
    // The client's own diagnosis names the missing capability…
    assert!(run.stderr.contains("strategy-verbs"), "{transcript}");
    assert!(run.stderr.contains("older vike-tradehub"), "{transcript}");
    // …and the CLI adds the action that fixes it.
    assert!(run.stderr.contains("upgrade the node at"), "{transcript}");

    // (2) THE DEGRADE: the mode is on stdout, from the node, under a verb that could not answer
    // the rest of what it promises.
    assert!(
        run.stdout.contains("trading state: Halted"),
        "an old node can still answer whether it is HALTED, and this verb must ask\n{transcript}"
    );
    assert!(
        run.stdout.contains("mounted strategies: UNAVAILABLE"),
        "…and must say plainly which half it could not answer\n{transcript}"
    );

    // (1) The scripted node's own observation, per connection.
    let logs = scripted_verdict(&rx, &transcript);
    assert_eq!(logs.len(), 2, "one connection per wire read:\n{logs:?}\n{transcript}");
    assert!(
        logs[0].is_empty(),
        "a frame arrived at the old node after the handshake — the registry refusal must be \
         client-side, with nothing sent: {:?}\n{transcript}",
        logs[0]
    );
    assert_eq!(logs[1], vec!["Snapshot".to_string()], "…and the second asks for the MODE");
}

/// **A mode read that FAILS may not vanish** — the `--json` half of the same rule.
///
/// The scripted node advertises `strategy-verbs` and answers the registry, then refuses the
/// snapshot with a node-side error. The document must carry `trading_state_error` (never simply
/// omit the mode, which `jq .trading_state` cannot tell from `null`) and the reason must ALSO reach
/// stderr, which is the stream a script's operator actually reads when the JSON looks odd.
#[test]
fn a_failed_mode_read_is_a_json_key_and_a_stderr_line() {
    let (addr, rx) = spawn_scripted_node(
        vec!["subscribe".to_string(), "strategy-verbs".to_string()],
        2,
        |req| match req {
            Request::StrategyStatus => {
                Some(Response::StrategyStatus(Box::new(scripted_registry())))
            }
            Request::Snapshot => {
                Some(Response::Error("snapshot unavailable: no core attached".to_string()))
            }
            _ => None,
        },
    );
    let settings = settings_dir_with_store(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)]);

    let run = run_cli(&addr.to_string(), settings.path(), &["--json"]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    // The REGISTRY answered, so the command succeeded — the mode is the best-effort half.
    assert!(run.ok, "the registry answered, so this run succeeds:\n{transcript}");

    let v: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}):\n{transcript}"));
    assert!(v.get("trading_state").is_none(), "no mode may be invented:\n{transcript}");
    assert!(
        v["trading_state_error"].as_str().unwrap_or_default().contains("no core attached"),
        "the failed read must be a KEY carrying the node's own reason:\n{transcript}"
    );
    assert_eq!(v["strategy_status"]["identity"]["name"], "e2e-hub", "{transcript}");
    // …and on stderr, because a `--json` stdout is a document rather than a transcript.
    assert!(
        run.stderr.contains("did not answer a snapshot"),
        "a failed read must not be silent on stderr either:\n{transcript}"
    );

    let logs = scripted_verdict(&rx, &transcript);
    assert_eq!(logs.len(), 2, "{logs:?}\n{transcript}");
    assert_eq!(logs[0], vec!["StrategyStatus".to_string()], "{logs:?}");
    assert_eq!(logs[1], vec!["Snapshot".to_string()], "{logs:?}");
}

/// No observe key anywhere: exit 1 with a message naming the key, both sources, and the store
/// path — the operator is pointed at where to put one, not told "nothing to do".
///
/// ⚠ The path it names is `node.env`, and that assertion is the point rather than a detail. An
/// operator reading this message has NOTHING to migrate, so naming `secrets.env` would send them to
/// write a key into the file the very next run tells them to move it out of — and
/// `vike-cli backend setup`, this binary's own writer, writes `node.env`. A refusal that names a
/// remedy the writer disagrees with is the defect class this whole split exists to close.
#[test]
fn with_no_observe_key_anywhere_the_message_names_both_sources() {
    // A store that EXISTS but holds no node key — the "present — but…" arm of the message.
    let settings = settings_dir_with_store(&[("SOME_OTHER_KEY", "not-a-node-key")]);

    let run = run_cli("127.0.0.1:1", settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "must fail:\n{transcript}");
    assert!(run.stderr.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{transcript}");
    assert!(run.stderr.contains("process environment"), "{transcript}");
    assert!(
        run.stderr.to_lowercase().contains(vike_secrets::NODE_FILE),
        "must name the store a key would go IN: {transcript}"
    );
    // No connection is attempted without a key, so nothing about the (dead) address is reported.
    assert!(!run.stderr.contains("127.0.0.1:1"), "{transcript}");
}

/// **The migration, end to end through the shipped binary.** A box whose observe key is still in
/// `secrets.env` — every box that was working before 2026-09-08 — must keep working, and must be
/// TOLD, once, where the key belongs now.
///
/// Both halves are load-bearing. Without the first, a deploy takes a running node's authentication
/// away; without the second, the fallback is silent and no box ever migrates, so the read can never
/// be deleted. `docs/decisions/0051-node-keys-live-in-their-own-store.md` is the record, and its
/// "reads fall back, writes do not" clause is what makes the notice actionable: the command it
/// names writes the new file rather than the one being fallen back to.
#[test]
fn legacy_keys_still_authenticate_and_say_so() {
    let (_mount, addr) = spawn_node(Some(identity()), mounts());
    let settings = settings_dir_with_store_in(
        vike_secrets::SECRETS_FILE,
        &[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)],
    );

    let run = run_cli(&addr.to_string(), settings.path(), &[]);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    // It WORKS: the fallback is a migration, not a deprecation warning that breaks the box.
    assert!(run.ok, "a legacy pair must still authenticate:\n{transcript}");
    assert!(run.stdout.contains("node e2e-hub"), "{transcript}");
    // …and it SAYS SO, on stderr — never stdout, which `--json` makes a document.
    assert!(
        run.stderr.contains(vike_secrets::NODE_FILE)
            && run.stderr.contains(vike_secrets::SECRETS_FILE),
        "the notice must name the file it read AND the file to move to:\n{transcript}"
    );
    assert!(
        !run.stdout.contains(vike_secrets::NODE_FILE),
        "stdout stays the answer:\n{transcript}"
    );
}
