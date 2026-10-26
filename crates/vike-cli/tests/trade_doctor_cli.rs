//! End-to-end proof that `vike-cli trade doctor` judges a node through the SHIPPED binary: the three
//! observe-scope reads, the table, the JSON document, and the exit number that is the verdict.
//!
//! Hermetic and loopback-only, the `tests/trade_status_cli.rs` shape, mostly against a SCRIPTED node
//! rather than a real paper one: a doctor's findings (a halted venue, a door that is still starting)
//! are states a fresh paper core never reaches. The scripted node runs the real client handshake
//! (Hello, Welcome, Auth, AuthOk), accepts the mac unseen, and answers the ONE request each
//! connection carries from a closure. The observe key is an obviously-fake `DUMMY-*` string written
//! to a throwaway settings database, deliberately the STORE and not the environment. One test runs
//! against a REAL node instead (the real publisher and server over a paper core), because the three
//! frame fields the doctor reads (`uptime_ms`, `halt_sentinel`, `age_ms`) are stamped by that
//! publisher and a scripted node would only prove the doctor reads what the test wrote.
//!
//! - [`a_healthy_node_reads_ok_and_exits_zero`] — all three reads happen (the engine row proves the
//!   directory was compared, the recon row the capabilities), the table ends in `DOCTOR: OK`, and
//!   the facts the wire does not carry are UNKNOWN rows, not silence. The scripted frame carries
//!   none of the newer fields, so it is also the OLDER NODE: its rows read `unknown (node too old)`.
//! - [`a_current_node_is_judged_on_its_uptime_its_oldest_order_and_the_sentinel`],
//!   [`an_engaged_sentinel_exits_two`], [`a_node_that_has_just_restarted_exits_one`] and
//!   [`a_resting_order_older_than_a_day_exits_one`] — the same rows against a node that publishes
//!   the fields, one verdict each.
//! - [`a_real_hub_reports_its_uptime_and_follows_the_sentinel_file`] — the fields through the real
//!   publisher and server: `touch` shows on the next read and `rm` clears it.
//! - [`json_is_one_document_with_the_verdict_and_every_row`] — the machine surface.
//! - [`a_halted_venue_exits_two_and_says_which`] — FAIL is the second number.
//! - [`a_starting_node_exits_one_and_carries_its_progress`] — the door that listens before the
//!   accounts are up is a WARN with where the start has got to, and nothing more is asked of it.
//! - [`an_unreachable_node_is_a_fail_verdict`] — a verdict, not a crash: the table and `DOCTOR:`
//!   line still come out.
//! - [`with_no_observe_key_nothing_is_dialled_and_the_rung_is_refused`] — not a verdict, so not on
//!   the verdict numbers.

mod common;

use std::net::{SocketAddr, TcpListener};
use std::process::Command;
use std::thread;

use vike_hub_proto::auth::{HubKeys, Scope};
use vike_tradehub::{publish, server};
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, read_frame, starting_error, write_frame,
};
use vike_tradehub_client::wire::{
    WireDirectory, WireDirectoryAccount, WireHubIdentity, WireSnapshot, WireStrategyStatus,
    WireVenueBlock,
};
use vike_tradehub_core::{MakerConfig, PaperMakerCore, build_paper_maker_core};

/// Obviously fake: an HMAC secret, so any bytes work as long as both sides agree.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-trade-doctor-e2e";

/// What a current node advertises that the doctor reads: the strategy read (capabilities), the
/// directory, and the held-row publication.
fn features() -> Vec<String> {
    ["strategy-verbs", "directory", "recon-verbs"].iter().map(|s| s.to_string()).collect()
}

fn identity() -> WireHubIdentity {
    WireHubIdentity {
        name: "e2e-hub".to_string(),
        strategy: "spread_maker".to_string(),
        params: String::new(),
        live: false,
        build: "vike-tradehub test-build".to_string(),
        advertise_addr: String::new(),
    }
}

/// One engine block of the default account of `venue` in `state` (`Active`, `Halted`, ...).
fn block(venue: &str, state: &str) -> WireVenueBlock {
    serde_json::from_value(serde_json::json!({
        "venue": venue, "balance": 1000.0, "realized_pnl": 0.0, "fees_paid": 0.0,
        "funding_paid": 0.0, "equity": 1000.0, "unrealized": 0.0, "missing_prices": 0,
        "margin_used": 0.0, "free_bp": 1000.0, "trading_state": state, "positions": [],
        "route_key": venue, "tier": "demo",
    }))
    .expect("a venue block")
}

/// A node that folded, running `venue` in `state`.
fn snapshot(venue: &str, state: &str) -> WireSnapshot {
    let mut snap = WireSnapshot::empty();
    snap.seq = 3;
    snap.identity = Some(identity());
    snap.venues = vec![block(venue, state)];
    snap
}

/// [`snapshot`] as a CURRENT node publishes it: up `uptime_ms`, no HALT sentinel file, and one
/// resting order for each of `order_ages_ms`. [`snapshot`] itself carries none of the three newer
/// fields, which is exactly an older node's frame.
fn current_snapshot(
    venue: &str,
    state: &str,
    uptime_ms: u64,
    order_ages_ms: &[u64],
) -> WireSnapshot {
    let mut snap = snapshot(venue, state);
    snap.uptime_ms = Some(uptime_ms);
    snap.halt_sentinel = Some(false);
    snap.orders = order_ages_ms
        .iter()
        .enumerate()
        .map(|(i, age)| {
            serde_json::from_value(serde_json::json!({
                "client_order_id": format!("c-{i}"), "venue": venue, "symbol": "BTCUSDT",
                "side": 1, "qty": 1.0, "order_type": "limit", "price": 100.0,
                "trigger_price": null, "status": "ACCEPTED", "venue_order_id": null,
                "filled_qty": 0.0, "avg_fill_px": 0.0, "age_ms": age,
            }))
            .expect("an order")
        })
        .collect();
    snap
}

/// The active rows the directory read answers: `venue`, default account, tier demo.
fn directory(venue: &str) -> WireDirectory {
    WireDirectory {
        venues: Vec::new(),
        accounts: vec![WireDirectoryAccount {
            id: 1,
            venue: venue.to_string(),
            label: None,
            tier: "demo".to_string(),
            venue_account_id: None,
        }],
    }
}

/// Serve `connections` connections, one request each, on an ephemeral loopback port. Each runs the
/// handshake the client really performs, accepts the mac unseen, and answers from `answer`.
fn spawn_node(
    connections: usize,
    answer: impl Fn(&Request) -> Response + Send + 'static,
) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("resolve the assigned port");
    thread::spawn(move || {
        for _ in 0..connections {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let Ok(Request::Hello { .. }) = read_frame::<_, Request>(&mut stream) else {
                continue;
            };
            let welcome = Response::Welcome {
                proto_version: NODE_PROTO_VERSION,
                nonce: [7u8; 32],
                features: features(),
            };
            if write_frame(&mut stream, &welcome).is_err() {
                continue;
            }
            let Ok(Request::Auth { .. }) = read_frame::<_, Request>(&mut stream) else {
                continue;
            };
            if write_frame(&mut stream, &Response::AuthOk { scope: Scope::Read }).is_err() {
                continue;
            }
            if let Ok(request) = read_frame::<_, Request>(&mut stream) {
                let _ = write_frame(&mut stream, &answer(&request));
            }
        }
    });
    addr
}

/// A node answering the three reads: `snap`, a registry with no mounts, and `venue`'s directory.
fn healthy_node(snap: WireSnapshot, venue: &str) -> SocketAddr {
    let status = WireStrategyStatus {
        identity: identity(),
        effective_params: String::new(),
        mounts: Vec::new(),
    };
    let directory = directory(venue);
    spawn_node(3, move |request| match request {
        Request::Snapshot => Response::SnapshotFrame(Box::new(snap.clone())),
        Request::StrategyStatus => Response::StrategyStatus(Box::new(status.clone())),
        Request::Directory => Response::Directory(Box::new(directory.clone())),
        other => Response::Error(format!("unscripted request: {other:?}")),
    })
}

/// A throwaway PROJECT and its `settings/` child, kept together so the `TempDir` outlives the path
/// handed to the child process. The settings directory is a CHILD, never the bare `TempDir`: the
/// child takes the parent of `$VIKE_SETTINGS_DIR` as its project, and a bare temp directory would
/// make that the shared system temp root (`tests/trade_status_cli.rs`'s `SettingsFixture` argues it).
struct Settings {
    _root: tempfile::TempDir,
    dir: std::path::PathBuf,
}

fn settings_with(keys: &[(&str, &str)]) -> Settings {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("settings");
    std::fs::create_dir_all(&dir).expect("create the settings directory");
    common::seed_store(&dir, keys);
    Settings { _root: root, dir }
}

struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Run {
    fn transcript(&self) -> String {
        format!("--- stdout ---\n{}\n--- stderr ---\n{}", self.stdout, self.stderr)
    }
}

/// Run the SHIPPED bin against `addr`. The hub-key variables are REMOVED from the child's
/// environment: the configuration under test is "key in the store, nothing exported".
fn run_cli(addr: &str, settings: &Settings, extra: &[&str]) -> Run {
    let out = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["trade", "doctor", "--node", addr])
        .args(extra)
        .env("VIKE_SETTINGS_DIR", &settings.dir)
        .env_remove("VIKE_USER_DATA_DIR")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .expect("run vike-cli trade doctor");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        code: out.status.code(),
    }
}

fn with_key() -> Settings {
    settings_with(&[("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY)])
}

#[test]
fn a_healthy_node_reads_ok_and_exits_zero() {
    let addr = healthy_node(snapshot("binance", "Active"), "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(0), "{t}");
    assert!(run.stdout.starts_with(&format!("vike-cli trade doctor: {addr}\n")), "{t}");
    assert!(run.stdout.contains("vike-tradehub test-build"), "{t}");
    // The directory read happened: the engine row compares its tier with the active row's.
    assert!(run.stdout.contains("RUNNING at tier demo (the active row agrees)"), "{t}");
    // The capabilities read happened: an empty held list is "nothing held", not "unknown".
    assert!(run.stdout.contains("nothing is held for an operator"), "{t}");
    // What the wire does not carry is said, with the command, and does not move the verdict.
    assert!(run.stdout.contains("unknown (not on the wire)"), "{t}");
    assert!(run.stdout.contains("systemctl show vike-tradehub -p ActiveEnterTimestamp"), "{t}");
    // This scripted frame is an OLDER node's (no uptime, sentinel or order age on it): the three
    // rows say so in their own words, and none of them is a guess.
    assert!(run.stdout.contains("unknown (node too old): how long the node has been up"), "{t}");
    assert!(run.stdout.contains("unknown (node too old): whether the HALT sentinel"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: OK"), "{t}");
    assert!(run.stderr.trim().is_empty(), "{t}");
}

#[test]
fn a_current_node_is_judged_on_its_uptime_its_oldest_order_and_the_sentinel() {
    // Up 1d 2h, with two resting orders: the older of them (3h) is what the row names.
    let hour = 60 * 60 * 1000;
    let snap = current_snapshot("binance", "Active", 26 * hour, &[hour / 2, 3 * hour]);
    let addr = healthy_node(snap, "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(0), "{t}");
    assert!(run.stdout.contains("up 1d 2h"), "{t}");
    assert!(run.stdout.contains("no HALT sentinel file"), "{t}");
    let oldest = "the oldest resting order is 3h 0m old: c-1 binance BTCUSDT";
    assert!(run.stdout.contains(oldest), "{t}");
    assert!(!run.stdout.contains("unknown (node too old)"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: OK"), "{t}");
}

#[test]
fn an_engaged_sentinel_exits_two() {
    let mut snap = current_snapshot("binance", "Active", 3_600_000, &[]);
    snap.halt_sentinel = Some(true);
    let addr = healthy_node(snap, "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(2), "{t}");
    assert!(run.stdout.contains("the HALT sentinel file EXISTS"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: FAIL 1"), "{t}");
}

#[test]
fn a_node_that_has_just_restarted_exits_one() {
    let addr = healthy_node(current_snapshot("binance", "Active", 20_000, &[]), "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "{t}");
    assert!(run.stdout.contains("up 20s: restarted recently"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: WARN 1"), "{t}");
}

#[test]
fn a_resting_order_older_than_a_day_exits_one() {
    let day = 24 * 60 * 60 * 1000;
    let snap = current_snapshot("binance", "Active", 3 * day, &[day + 1000]);
    let addr = healthy_node(snap, "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "{t}");
    assert!(run.stdout.contains("1d 0h old (over 24h): c-0 binance BTCUSDT"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: WARN 1"), "{t}");
}

/// A REAL node: the paper core, the real publisher and the real server, with the publisher handed a
/// sentinel path as the daemon's `start_observe_server` hands it. The doctor's `uptime` and
/// `halt-sentinel` rows are then the frame the publisher stamped, read over the wire:
/// no file is a PASS, a `touch` is a FAIL on the next read, and the `rm` clears it again.
#[test]
fn a_real_hub_reports_its_uptime_and_follows_the_sentinel_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sentinel = dir.path().join("HALT");
    let (_mount, addr) = spawn_real_hub(&sentinel);
    let settings = with_key();

    let read = || {
        let run = run_cli(&addr.to_string(), &settings, &["--json"]);
        let doc: serde_json::Value = serde_json::from_str(&run.stdout)
            .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", run.transcript()));
        (run, doc)
    };

    // No file: the sentinel row is a PASS, and the uptime is READ (PASS or WARN by how long this
    // test process has been up), never an UNKNOWN row.
    let (run, doc) = read();
    let t = run.transcript();
    let (status, detail) = row(&doc, "halt-sentinel");
    assert_eq!(status, "PASS", "{t}");
    assert_eq!(detail, "no HALT sentinel file", "{t}");
    let (status, detail) = row(&doc, "uptime");
    assert!(status == "PASS" || status == "WARN", "{t}");
    assert!(detail.starts_with("up "), "{t}");

    // `touch`: the next read says so, and the verdict is the second number.
    std::fs::write(&sentinel, b"").expect("touch the sentinel");
    let (run, doc) = read();
    let t = run.transcript();
    assert_eq!(row(&doc, "halt-sentinel").0, "FAIL", "{t}");
    assert_eq!(run.code, Some(2), "{t}");

    // `rm`: nothing latches.
    std::fs::remove_file(&sentinel).expect("rm the sentinel");
    let (run, doc) = read();
    assert_eq!(row(&doc, "halt-sentinel").0, "PASS", "{}", run.transcript());
}

/// The `(status, detail)` of the first `check` row of a `--json` document.
fn row(doc: &serde_json::Value, check: &str) -> (String, String) {
    let rows = doc["checks"].as_array().expect("an array of rows");
    let found = rows.iter().find(|r| r["check"] == check);
    let found = found.unwrap_or_else(|| panic!("no `{check}` row in {doc:#}"));
    let text = |key: &str| found[key].as_str().unwrap_or_default().to_string();
    (text("status"), text("detail"))
}

/// A real paper node on an ephemeral loopback port, observe scope only, whose publisher watches
/// `sentinel`. The returned mount keeps the core alive.
fn spawn_real_hub(sentinel: &std::path::Path) -> (PaperMakerCore, SocketAddr) {
    let cfg =
        MakerConfig::outcome_token("polymarket", "TRADE_DOCTOR_E2E_TOKEN", Some(3_000_000_000));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind an ephemeral loopback port");
    let addr = listener.local_addr().expect("resolve the assigned port");
    let publisher =
        publish::spawn_with_mounts(mount.handle.snapshot_cell(), Some(identity()), vec![], vec![])
            .with_halt_sentinel_path(sentinel.to_path_buf());
    let keys = HubKeys::new(OBSERVE_KEY.as_bytes().to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::control::ControlLimitsConfig::default(),
            None,
            None,
            None,
        );
    });
    (mount, addr)
}

#[test]
fn json_is_one_document_with_the_verdict_and_every_row() {
    let addr = healthy_node(snapshot("binance", "Active"), "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &["--json"]);
    let t = run.transcript();
    assert_eq!(run.code, Some(0), "{t}");
    let doc: serde_json::Value = serde_json::from_str(&run.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {t}"));
    assert_eq!(doc["verdict"], "OK", "{t}");
    assert_eq!(doc["fail"], 0, "{t}");
    let checks = doc["checks"].as_array().expect("an array of rows");
    assert_eq!(checks[0]["check"], "door", "{t}");
    assert_eq!(checks[0]["status"], "PASS", "{t}");
    assert!(checks.iter().any(|c| c["status"] == "UNKNOWN"), "{t}");
}

#[test]
fn a_halted_venue_exits_two_and_says_which() {
    let addr = healthy_node(snapshot("binance", "Halted"), "binance");
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(2), "{t}");
    assert!(run.stdout.contains("HALTED"), "{t}");
    assert!(run.stdout.contains("FAIL"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: FAIL 1"), "{t}");
}

#[test]
fn a_starting_node_exits_one_and_carries_its_progress() {
    let progress = "starting the accounts; accounts: binance up, dukascopy starting";
    let addr = spawn_node(1, move |_| Response::Error(starting_error(progress)));
    let run = run_cli(&addr.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(1), "{t}");
    assert!(run.stdout.contains("STARTING"), "{t}");
    assert!(run.stdout.contains(progress), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: WARN 1"), "{t}");
}

#[test]
fn an_unreachable_node_is_a_fail_verdict() {
    // A port that was free a moment ago and has no listener now: the dial is refused at once.
    let closed = TcpListener::bind("127.0.0.1:0").expect("bind").local_addr().expect("addr");
    let run = run_cli(&closed.to_string(), &with_key(), &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(2), "{t}");
    assert!(run.stdout.contains("cannot read a snapshot from"), "{t}");
    assert_eq!(run.stdout.lines().last(), Some("DOCTOR: FAIL 1"), "{t}");
}

#[test]
fn with_no_observe_key_nothing_is_dialled_and_the_rung_is_refused() {
    // A store that EXISTS but holds no hub key; the address is dead and must never be named.
    let settings = settings_with(&[("SOME_OTHER_KEY", "not-a-hub-key")]);
    let run = run_cli("127.0.0.1:1", &settings, &[]);
    let t = run.transcript();
    assert_eq!(run.code, Some(4), "no key is a refusal, not a verdict: {t}");
    assert!(run.stderr.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{t}");
    assert!(run.stdout.trim().is_empty(), "no table without a verdict: {t}");
    assert!(!run.stderr.contains("127.0.0.1:1"), "{t}");
}
