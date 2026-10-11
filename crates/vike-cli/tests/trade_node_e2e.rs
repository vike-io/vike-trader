//! End-to-end proof that `vike-cli trade` can actually PLACE AN ORDER — the two defects a clean
//! install found, gated together because one user hits them in sequence in one session.
//!
//! Hermetic and loopback-only, the same shape as `tests/backtest_cli.rs` one layer up: a REAL
//! PAPER node (`vike_mount::build_paper_maker_core` + the real `vike_tradehub::{server, publish}`,
//! the harness `crates/vike-tradehub/tests/control_roundtrip.rs` established) bound on an ephemeral
//! `127.0.0.1:0` port, driven by the SHIPPED bin via `CARGO_BIN_EXE_vike-cli`. No credentials, no
//! venue, no external network; the HMAC keys are obviously-fake `DUMMY-*` strings written into a
//! throwaway settings directory. The paper mount has no feed, so the maker never quotes and the
//! ONLY orders in the node's book are the ones the REPL placed.
//!
//! # What each test would have caught
//!
//! - [`repl_submit_places_an_order_end_to_end`] — **both defects at once, through the shipped
//!   binary.** The node keys exist ONLY in the node-key store (the process environment is
//!   explicitly scrubbed of them), so a client that reads `std::env::var` and nothing else prints
//!   "nothing to do" and exits 1 — DEFECT 2. Past that, the `submit` line must arrive at the node
//!   with a pre-minted `client_order_id`, or the server answers "remote submit requires a
//!   pre-minted client_order_id" and books nothing — DEFECT 1. The assertion is not the printed
//!   line alone: the order is read back off a SEPARATE observe connection to the node's own pushed
//!   snapshot, so it proves an order exists, not that a message was formatted.
//! - [`repl_with_no_key_anywhere_names_the_store_it_looked_in`] — the honest failure. With neither
//!   source carrying a key the command still exits 1, but the message now NAMES the store path, so
//!   the operator can see where to put one. The old message said "not set in the environment" while
//!   the daemon's own keys sat unread in that file.
//! - [`two_submits_in_one_session_book_two_distinct_orders`] — the mint is per ORDER, not per
//!   session. The node's registry is coid-keyed and idempotent, so a repeated id would book ONE
//!   order and silently drop the second; only counting the orders at the node catches that.
//! - [`a_node_with_control_disabled_says_so_instead_of_blaming_the_key`] — the write refusal named
//!   the wrong cause. With the node's `tradehub_control` off and a VALID key in the store, every
//!   write printed "no VIKE_TRADEHUB_CONTROL_KEY", sending the operator after a key that was never
//!   the problem. The wiring is what broke — all three causes produced `control: None` — so only a
//!   real refused handshake proves the right one is reported.
//! - [`submit_refuses_an_unmintable_coid_while_cancel_warns_and_still_sends`] — `submit` and
//!   `cancel` taught different rules about the same field. Both speak about the charset now, with
//!   deliberately different force; the test pins BOTH halves, because making them agree by
//!   tightening `cancel` would leave an order placed through another surface uncancellable here.
//! - [`the_preview_prints_typed_numbers_without_a_float_tail`] — `0.4 * 3` reached the guardrail
//!   line as `1.2000000000000002`. The renderer's own tests live in `cmd/verbs.rs`; this proves the
//!   REPL uses it, on both preview lines.
//!
//! # The ONE-SHOT kill switch (`vike-cli trade halt` / `resume`)
//!
//! ⚠ **These three exist because the write path they drive shipped with no test that reached it.**
//! Ruling 17 gave `halt`/`resume` their own argv verbs — a command a runbook or a unit file can run
//! against a LIVE order-signing daemon — and every test of it was a unit test of the PARSER. A
//! parser test cannot see the four things that actually stop a node: that the resolved command
//! reaches the wire, that the node applies it, that the confirm gate is between them, and what the
//! exit code is. All three below run the SHIPPED binary against the same in-process paper node the
//! REPL tests use, and each asserts the mode the node ENDS IN — read back off an independent
//! observe connection, never off a printed line.
//!
//! - [`the_one_shot_halt_and_resume_change_the_nodes_trading_mode`] — the write path end to end,
//!   both directions, with the exit code. A `halt` that printed "accepted" and left the node Active
//!   passes every other test in this repository.
//! - [`a_one_shot_halt_with_a_closed_stdin_aborts_and_halts_nothing`] — the confirm gate: EOF is a
//!   NO, the exit is non-zero, and **the node is still Active afterwards**. Asserting the printed
//!   "aborted" line alone would pass on a build that sent the command and then said otherwise.
//! - [`a_piped_confirmation_halts_the_node_without_yes`] — the TRUE behaviour of a non-terminal
//!   stdin, pinned because the shipped `--help` and `docs/ops/kill-switches.md` both claimed the
//!   opposite ("a piped stdin reads as NO") until 2026-09-10. It is a `read_line`: `y` on a pipe
//!   confirms. This test fails the day that stops being true, which is the day those words have to
//!   change back — see `crates/vike-cli/src/cmd/trade/write.rs`'s `confirm_stdin`.
//!
//! # A settings write asks ONE question (`docs/decisions/0086` point 7)
//!
//! - [`a_policy_set_setting_goes_out_after_one_confirm`] — the REPL used to ask a SECOND question
//!   after the `y/N`, a retype of the key that `--yes` could not answer. Over a pipe that prompt ate
//!   whatever line came next, so a `set-setting …` / `y` / `quit` session wrote NOTHING. Judged by
//!   the ROW on the node's own settings database, not by a printed line — and the `old → new` line
//!   the operator confirms against must print before the write's result does.
//! - [`a_policy_set_setting_under_yes_goes_out_with_no_prompt`] — `--yes` covers the settings
//!   write like every other write verb; nothing is left for a script to be unable to answer.
//!
//! ⚠ These drive the REPL over a PIPE, not a terminal. `rustyline` falls back to plain line reads
//! when stdin is not a tty, which is what makes the shipped binary testable at all; `--yes` skips
//! the interactive confirm. A pipe also means the harness's own writes RACE the child, and one of
//! these tests expects the binary to be GONE before it reads anything — so [`run_trade`] takes a
//! [`Liveness`] declaring which case it is. See that enum for why a blanket "ignore `BrokenPipe`"
//! would be the wrong fix.

use std::io::Write;
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use vike_mount::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::wire::WireTradingState;
use vike_tradehub_client::{NodeKeys, RemoteCoreHandle};

mod common;
use common::wait_until;

/// The paper mount's symbol. Any string works — nothing resolves it against a venue.
const TOKEN: &str = "CLI_TRADE_E2E_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the vike-mount offline mount test).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// Obviously-fake keys. They are HMAC secrets, so any bytes work as long as both sides agree — and
/// the `DUMMY-` prefix makes it unmistakable in a diff or a process listing that no real credential
/// is involved.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-vike-cli-e2e";
const CONTROL_KEY: &str = "DUMMY-control-key-for-the-vike-cli-e2e";

/// A resting limit far from any market: with no feed it stays WORKING forever, so the node's
/// snapshot deterministically carries exactly what the REPL placed.
const SUBMIT_LINE: &str = "submit polymarket CLI_TRADE_E2E_TOKEN buy 20 @0.40";

/// Build a PAPER node and serve it on an ephemeral loopback port with both scopes keyed and the
/// core's `CommandSink` threaded in (control ENABLED). The returned mount keeps the core alive.
fn spawn_node() -> (MakerMount, SocketAddr) {
    spawn_node_with_control(true)
}

/// [`spawn_node`], with the node's CONTROL scope on or off.
///
/// `control_enabled = false` reproduces a node started WITHOUT `flags.tradehub_control`
/// exactly as the daemon does it: `crates/vike-tradehub/src/node.rs`'s `start_observe_server`
/// ZEROES the control key before the server ever sees it, so `run_handshake` refuses a `Scope::Write` request without consulting
/// anything. The client still holds a perfectly good key — which is the entire point of the case.
fn spawn_node_with_control(control_enabled: bool) -> (MakerMount, SocketAddr) {
    // No settings source: these cases drive the TRADE path; the SettingsShow arm's answer shape is
    // pinned in vike-tradehub's tests/daemon/settings_show.rs.
    spawn_node_serving(control_enabled, None)
}

/// [`spawn_node`], with the node serving a SETTINGS source over `settings_dir` — the node-side
/// store a `set-setting` line writes into (`docs/decisions/0086`: one row in
/// `<settings_dir>/db/vike.db`), read and written by the same arm the shipped daemon runs.
fn spawn_node_with_settings(settings_dir: &Path) -> (MakerMount, SocketAddr) {
    spawn_node_serving(
        true,
        Some(server::settings::SettingsShowSource {
            settings_dir: Some(settings_dir.to_path_buf()),
            // No hot-apply seam: every key these cases write is restart-class anyway (policy is
            // never hot), which is the `applies at the next restart` answer they read back.
            hot: None,
            journal: None,
        }),
    )
}

/// The one node spawner the two above share, so a change to how a node is stood up is made once.
fn spawn_node_serving(
    control_enabled: bool,
    settings: Option<server::settings::SettingsShowSource>,
) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(mount.handle.snapshot_cell(), None);
    let commands = Some(mount.handle.command_sink());
    let control_key = if control_enabled { CONTROL_KEY.as_bytes().to_vec() } else { Vec::new() };
    let keys = NodeKeys::new(OBSERVE_KEY.as_bytes().to_vec(), control_key);
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            settings,
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            None,
        );
    });
    (mount, addr)
}

/// The NODE's own settings store, seeded with ONE policy row so a write has a real OLD value to
/// report. A separate directory from the CLIENT's key store ([`settings_dir_with_store`]), the way a
/// real node's `<project>/settings` is on a different box from the operator's.
fn node_settings_with_ceiling(value: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    vike_secrets::plant_settings_rows(
        dir.path(),
        &vike_secrets::StoredSettings {
            settings: vec![vike_secrets::SettingRow {
                section: "policy".into(),
                key: "max_notional_per_order".into(),
                value: value.into(),
            }],
            venue: Vec::new(),
        },
    )
    .expect("seed the node's settings database");
    dir
}

/// What the node's settings DATABASE holds for the ceiling row — the node's truth, read off the
/// store itself rather than off anything the REPL printed.
fn stored_ceiling(dir: &Path) -> Option<String> {
    let source = vike_secrets::read_settings_in(dir).expect("read the node's settings database");
    let rows = &source.rows().expect("rows").settings;
    rows.iter()
        .find(|r| r.section == "policy" && r.key == "max_notional_per_order")
        .map(|r| r.value.clone())
}

/// A throwaway `<project>/settings` directory whose settings database holds the node keys —
/// and NOTHING else. This is the whole point of the second defect: the daemon reads its keys here,
/// so the client must too.
fn settings_dir_with_store(keys: &[(&str, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    common::seed_store(dir.path(), keys);
    dir
}

/// What one `vike-cli trade` run produced.
struct Run {
    stdout: String,
    stderr: String,
    ok: bool,
}

/// Whether the child is expected to be READING when the harness feeds it the session's lines.
///
/// This is the one fact that decides whether a closed stdin is a normal outcome or a real defect,
/// and it is a statement about what the binary under test is supposed to DO — not a tolerance knob
/// and not a retry. Writing to a pipe whose reader has exited fails with `BrokenPipe`; swallowing
/// that everywhere would also swallow "the session died with lines still unwritten", which is
/// precisely the class of defect these tests exist to catch.
#[derive(Clone, Copy)]
enum Liveness {
    /// The child resolves a key, connects, and sits in the REPL's read loop until it is told to
    /// quit — so every line the harness writes MUST land. A `BrokenPipe` here means the session
    /// died early, and [`run_trade`] fails with the child's own transcript attached.
    ReadsEveryLine,
    /// The child is expected to EXIT BEFORE it reads anything: `vike-cli trade`'s `!keys.has_any()`
    /// arm (`crates/vike-cli/src/cmd/trade.rs`'s `run`) prints its message and returns
    /// `ExitCode::FAILURE` above the connects and above the REPL. The harness's `quit` is therefore
    /// racing an exit that is the whole POINT of the test, and which side wins is pure scheduling:
    /// measured on the CI box at 0/36 failures on an idle 32-core box and 24/36 with the same 12-way
    /// soak pinned to two cores. So `BrokenPipe` — and ONLY `BrokenPipe` — is a normal outcome
    /// here; the harness stops writing and the run is judged on the exit status and the transcript,
    /// which is all this case asserts anyway. That is what bounds the tolerance: a child that died
    /// for some OTHER reason still closes the pipe, but it does not print the store path, so the
    /// caller's own assertions fail on the transcript rather than on the write.
    ExitsBeforeReading,
}

/// Run the SHIPPED `vike-cli trade` bin against `addr`, feeding `lines` (then `quit`) on stdin and
/// reading the session's output back. `liveness` says whether the child is expected to still be
/// reading while that happens.
///
/// The two node-key variables are explicitly REMOVED from the child's environment: this test would
/// pass vacuously on a developer box that happens to export them, and the configuration under test
/// is precisely "keys in the store, nothing in the environment".
fn run_trade(
    addr: SocketAddr,
    settings_dir: &std::path::Path,
    lines: &[&str],
    liveness: Liveness,
) -> Run {
    run_trade_with(addr, settings_dir, &["--yes"], lines, liveness)
}

/// [`run_trade`] with the REPL's own flags spelled by the caller. `&[]` is a session WITHOUT
/// `--yes`, which is the only way a test reaches the `confirm? [y/N]` prompt at all — every other
/// case in this file skips it, and each `y`/`n` then has to be one of `lines`.
fn run_trade_with(
    addr: SocketAddr,
    settings_dir: &std::path::Path,
    flags: &[&str],
    lines: &[&str],
    liveness: Liveness,
) -> Run {
    let node = addr.to_string();
    let mut argv = vec!["trade", "--node", node.as_str()];
    argv.extend_from_slice(flags);
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(&argv)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        // The advisory preview guardrail's one remaining env knob — cleared so a developer box's
        // value cannot flag the test order as over-limit and change the printed lines.
        .env_remove("VIKE_MAX_ORDER_QTY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vike-cli trade");
    // Taken OUT of the child rather than borrowed, so the failure arm below can reap the child for
    // its transcript, and so the write end is closed here by name — EOF is the REPL's other stop
    // trigger, and leaving it to `wait_with_output` would hide that.
    let mut stdin = child.stdin.take().expect("piped stdin");
    for line in lines.iter().copied().chain(std::iter::once("quit")) {
        match writeln!(stdin, "{line}") {
            Ok(()) => {}
            // The child was SUPPOSED to be gone. Stop writing (every later line meets the same
            // closed pipe) and let the exit status and the transcript carry the verdict.
            Err(e)
                if e.kind() == std::io::ErrorKind::BrokenPipe
                    && matches!(liveness, Liveness::ExitsBeforeReading) =>
            {
                break;
            }
            // The child was supposed to be READING. Reap it and report with its own output, which
            // is the only thing that says why it died.
            Err(e) => {
                drop(stdin);
                let out = child.wait_with_output().expect("wait for vike-cli trade");
                panic!(
                    "the REPL's stdin closed with {line:?} still unwritten ({e}) — the session was \
                     expected to be reading, so it exited early with {}\n--- stdout ---\n{}\n\
                     --- stderr ---\n{}",
                    out.status,
                    String::from_utf8_lossy(&out.stdout),
                    String::from_utf8_lossy(&out.stderr)
                );
            }
        }
    }
    drop(stdin);
    let out = child.wait_with_output().expect("wait for vike-cli trade");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        ok: out.status.success(),
    }
}

/// **BOTH defects, end to end, through the shipped binary.**
///
/// Keys only in the node-key store; an order typed at the REPL; the order read back off the
/// node's own pushed snapshot with the coid the REPL printed.
#[test]
fn repl_submit_places_an_order_end_to_end() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    let run = run_trade(addr, settings.path(), &[SUBMIT_LINE], Liveness::ReadsEveryLine);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);

    // DEFECT 2: the keys resolved from the store, so both halves connected.
    assert!(run.ok, "the session must exit cleanly\n{transcript}");
    assert!(
        run.stdout.contains("connected: CONTROL (write)"),
        "the control key must resolve from the node-key store\n{transcript}"
    );
    assert!(
        run.stdout.contains("node-key store"),
        "the connection line must say WHERE the key came from\n{transcript}"
    );

    // DEFECT 1: the node ACCEPTED the submit, which it cannot do without a pre-minted coid.
    assert!(
        run.stdout.contains("accepted by the node"),
        "the node must accept the submit\n{transcript}"
    );
    assert!(
        !run.stdout.contains("requires a pre-minted client_order_id"),
        "the server's empty-coid refusal must not appear\n{transcript}"
    );

    // …and the coid the operator was shown is a real, venue-valid id — not a placeholder.
    let coid = coid_from_preview(&run.stdout)
        .unwrap_or_else(|| panic!("the preview must print coid=<id>\n{transcript}"));
    assert!(
        vike_model::is_valid_crypto_coid(&coid),
        "the printed coid must be venue-valid: {coid:?}\n{transcript}"
    );

    // THE proof: an order actually exists at the node, under exactly that id. A printed line is
    // not an order.
    let observer =
        RemoteCoreHandle::connect(addr, OBSERVE_KEY.as_bytes()).expect("observe connect");
    assert!(
        wait_until(10, || observer.snapshot().orders.iter().any(|o| o.client_order_id == coid)),
        "the submitted order never reached the node's snapshot under coid {coid}\n{transcript}"
    );
    let snap = observer.snapshot();
    let order = snap.orders.iter().find(|o| o.client_order_id == coid).expect("order present");
    assert_eq!(order.symbol, TOKEN, "routed to the mount symbol");
    assert_eq!(order.side, 1, "side survived the REPL -> wire -> lower round-trip");
    assert_eq!(order.qty, 20.0);
}

/// The mint is per ORDER. The node's registry is coid-keyed and idempotent, so two submits sharing
/// an id would book ONE order and drop the other silently — counting at the node is the only way to
/// see it.
#[test]
fn two_submits_in_one_session_book_two_distinct_orders() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    let run =
        run_trade(addr, settings.path(), &[SUBMIT_LINE, SUBMIT_LINE], Liveness::ReadsEveryLine);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert_eq!(
        run.stdout.matches("accepted by the node").count(),
        2,
        "both submits must be accepted\n{transcript}"
    );

    let observer =
        RemoteCoreHandle::connect(addr, OBSERVE_KEY.as_bytes()).expect("observe connect");
    assert!(
        wait_until(10, || observer.snapshot().orders.len() >= 2),
        "two submits must book TWO orders (a repeated coid books one)\n{transcript}"
    );
    let snap = observer.snapshot();
    let ids: std::collections::BTreeSet<&str> =
        snap.orders.iter().map(|o| o.client_order_id.as_str()).collect();
    assert_eq!(ids.len(), snap.orders.len(), "every booked order has its own coid: {ids:?}");
}

/// No key in EITHER source: still a clean failure, but one that names the store it looked in.
///
/// This is the one case whose child is expected to be GONE before the harness writes `quit` — see
/// [`Liveness::ExitsBeforeReading`]. The assertions below are what bound that tolerance: they still
/// demand a non-zero exit AND the store path in the message, so a child that closed the pipe for
/// any other reason fails here, on the transcript.
#[test]
fn repl_with_no_key_anywhere_names_the_store_it_looked_in() {
    let (_mount, addr) = spawn_node();
    // A store that EXISTS but holds no node key: the unrelated key is a VENUE-credential row.
    let settings = tempfile::tempdir().expect("tempdir");
    common::seed_store(settings.path(), &[("SOME_OTHER_VENUE_API_KEY", "DUMMY-unrelated")]);

    let run = run_trade(addr, settings.path(), &[], Liveness::ExitsBeforeReading);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "no key anywhere is still a failure\n{transcript}");
    assert!(
        run.stderr.contains("node-key store"),
        "the message must name the store, not only the environment\n{transcript}"
    );
    assert!(
        run.stderr.contains(vike_secrets::DB_FILE),
        "…and name the STORE it read (the settings database)\n{transcript}"
    );
    assert!(run.stderr.contains("VIKE_TRADEHUB_OBSERVE_KEY"), "{transcript}");
    assert!(run.stderr.contains("VIKE_TRADEHUB_CONTROL_KEY"), "{transcript}");
}

/// **A node with CONTROL OFF and a valid key must not be reported as a missing key.**
///
/// The exact clean-install configuration: `tradehub_control` unset at the node, a perfectly good
/// `VIKE_TRADEHUB_CONTROL_KEY` in the node-key store. Every write printed
/// `writes disabled (OBSERVE-ONLY): no VIKE_TRADEHUB_CONTROL_KEY — nothing was sent`, which is
/// FALSE in every part except the last four words: the key resolved, authenticated, and was refused
/// for its SCOPE. The true reason had already been printed once at connect and thrown away.
///
/// It has to be an end-to-end test through the shipped binary, not a unit test on the message: what
/// broke was the WIRING — `run_write` branched on `control.is_none()`, which all three causes
/// produce — so only a real refused handshake proves the right one is now reported.
#[test]
fn a_node_with_control_disabled_says_so_instead_of_blaming_the_key() {
    let (_mount, addr) = spawn_node_with_control(false);
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    // The observe half still connects, so the session lives and reads every line.
    let run = run_trade(addr, settings.path(), &[SUBMIT_LINE], Liveness::ReadsEveryLine);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);

    // The premise: the key WAS there and the node refused the scope.
    assert!(
        run.stdout.contains("connected: OBSERVE (read)"),
        "the same store's observe key must still work — this is not a broken store\n{transcript}"
    );
    assert!(
        run.stderr.contains("control auth denied"),
        "the node must have refused the CONTROL scope for this case to mean anything\n{transcript}"
    );

    // The defect: the write refusal blamed the key.
    assert!(
        !run.stdout.contains("no VIKE_TRADEHUB_CONTROL_KEY"),
        "the write refusal must not claim the key is missing — it is present and it \
         authenticated\n{transcript}"
    );
    // The fix: it names what actually happened, and what to do about it.
    assert!(
        run.stdout.contains("REFUSED the control scope"),
        "the write refusal must name the real cause\n{transcript}"
    );
    assert!(
        run.stdout.contains("tradehub_control"),
        "…and the flag that fixes it, at the node\n{transcript}"
    );
    assert!(
        run.stdout.contains("nothing was sent"),
        "…while still stating the outcome that matters\n{transcript}"
    );
}

/// **`cancel` and `submit` no longer teach different rules about the same field.**
///
/// `submit --coid MYPINNED-001` was rejected with a good message; `cancel MYPINNED-001`, for an
/// order that never existed, answered `accepted by the node (coid MYPINNED-001)`. Both surfaces
/// speak about the charset now — and they speak with deliberately different force, which this pins:
/// `submit` REFUSES (it is minting an id, so nothing is lost by insisting) while `cancel` WARNS AND
/// STILL SENDS. Refusing a cancel would make an order placed through another surface — the `mcp`
/// tools pass an agent-supplied `client_order_id` through unvalidated — uncancellable from the
/// operator's console to enforce a client-side convention.
#[test]
fn submit_refuses_an_unmintable_coid_while_cancel_warns_and_still_sends() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    let run = run_trade(
        addr,
        settings.path(),
        &[
            "submit polymarket CLI_TRADE_E2E_TOKEN buy 20 @0.40 --coid MYPINNED-001",
            "cancel MYPINNED-001",
        ],
        Liveness::ReadsEveryLine,
    );
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "the session must exit cleanly\n{transcript}");

    // submit: refused, client-side, with the charset stated.
    assert!(
        run.stdout.contains("is not a usable client_order_id"),
        "submit must still refuse an id no venue would take\n{transcript}"
    );
    assert!(run.stdout.contains("alphanumeric"), "…and say what the charset is\n{transcript}");

    // cancel: warned, then SENT — the node's answer is what the operator sees.
    assert!(
        run.stdout.contains("not a client_order_id this node could have minted"),
        "cancel must say the id could not have been minted here\n{transcript}"
    );
    assert!(
        run.stdout.contains("sending it anyway"),
        "…and be explicit that it is still sent — cancel is fire-and-forget\n{transcript}"
    );
    assert!(
        run.stdout.contains("accepted by the node"),
        "…which means the node still gets it: refusing locally would make an order placed by \
         another surface uncancellable from here\n{transcript}"
    );
}

/// The preview's numbers carry no floating-point tail, through the shipped binary.
///
/// `0.4 * 3` is `1.2000000000000002`, and the guardrail line printed all seventeen digits. The unit
/// tests in `cmd/verbs.rs` pin the renderer; this pins that the renderer is what the REPL actually
/// uses, on BOTH lines of the preview — a `qty` formatted one way above a `qty` formatted another
/// would leave the operator choosing which of the tool's own lines to believe.
#[test]
fn the_preview_prints_typed_numbers_without_a_float_tail() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    let run = run_trade(
        addr,
        settings.path(),
        &["submit polymarket CLI_TRADE_E2E_TOKEN buy 3 @0.4"],
        Liveness::ReadsEveryLine,
    );
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "the session must exit cleanly\n{transcript}");

    assert!(
        run.stdout.contains("notional=1.2 "),
        "the guardrail must print the notional as typed\n{transcript}"
    );
    assert!(
        !run.stdout.contains("1.2000000000000002"),
        "no float tail may reach the preview\n{transcript}"
    );
    // Both preview lines agree on the same quantity.
    assert!(run.stdout.contains("qty=3 "), "the SUBMIT line's qty\n{transcript}");
    assert!(run.stdout.contains("guardrail: qty=3 "), "…and the guardrail's\n{transcript}");
    assert!(run.stdout.contains("@ 0.4"), "the price too\n{transcript}");
}

// ── the one-shot kill switch ─────────────────────────────────────────────────────────────────────

/// Run the SHIPPED bin ONE-SHOT — `vike-cli trade <verb> …`, no REPL — feeding `stdin` verbatim and
/// closing the pipe afterwards.
///
/// ⚠ Separate from [`run_trade`] rather than a flag on it, because the two differ in the one thing
/// that matters here: [`run_trade`] appends `quit` and passes `--yes` for the session, and both of
/// those would silence exactly the gate these tests exist to exercise. `stdin` is written and the
/// pipe is then dropped, so `""` IS the EOF case — the child sees a closed stdin with nothing on it.
fn run_trade_once(
    addr: SocketAddr,
    settings_dir: &std::path::Path,
    args: &[&str],
    stdin: &str,
) -> Run {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(["trade"])
        .args(args)
        .arg("--node")
        .arg(addr.to_string())
        .env("VIKE_SETTINGS_DIR", settings_dir)
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        .env_remove("VIKE_MAX_ORDER_QTY")
        // The two REMOVED risk variables refuse startup when set — cleared so an operator box's
        // stale export cannot fail a kill-switch test for an unrelated reason.
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn vike-cli trade one-shot");
    {
        let mut pipe = child.stdin.take().expect("piped stdin");
        // A one-shot that took `--yes` never reads, so its pipe may already be gone. That is the
        // expected shape for that case and not a failure of anything — the verdict is the exit
        // status, the transcript, and (above all) the mode the NODE ends in.
        let _ = pipe.write_all(stdin.as_bytes());
    }
    let out = child.wait_with_output().expect("wait for vike-cli trade one-shot");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        ok: out.status.success(),
    }
}

/// The node's OWN trading mode, off an independent observe connection — never a printed line.
///
/// ⚠ This is the whole point of these tests. `send_and_report` prints "accepted by the node" on the
/// node's ACK, which is the node saying it received a command, not the core saying it applied one;
/// and the mode is what a kill switch is FOR. Polling is required for the same reason the order
/// tests poll: the core folds on one thread and the publisher fans out on another.
///
/// `secs` is a parameter because the two directions are not the same question. Waiting for a mode
/// to ARRIVE wants a generous bound (a slow box, not a bug). Proving one NEVER arrives is a bound on
/// how long a command that should not exist is given to show up, and a generous one there is pure
/// wall clock in every passing run.
fn node_trading_state(observer: &RemoteCoreHandle, want: WireTradingState, secs: u64) -> bool {
    wait_until(secs, || observer.snapshot().trading_state == want)
}

/// The bound for "this mode must ARRIVE" — the same 10s the order assertions use.
const MODE_ARRIVES: u64 = 10;
/// The bound for "this mode must NEVER arrive". A command the CLI decided not to send has already
/// not been written to the socket by the time the child exits, so anything past a publish round
/// trip is only wall clock; 2s is ~100 poll passes of headroom over a localhost fold.
const MODE_STAYS_AWAY: u64 = 2;

/// **The one-shot write path, end to end, in both directions.**
///
/// `vike-cli trade halt --yes` must leave the NODE Halted and exit 0; `resume` must put it back.
/// Nothing below this line trusts a message: the assertion is the mode the node itself publishes.
#[test]
fn the_one_shot_halt_and_resume_change_the_nodes_trading_mode() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);
    let observer =
        RemoteCoreHandle::connect(addr, OBSERVE_KEY.as_bytes()).expect("observe connect");
    assert!(
        node_trading_state(&observer, WireTradingState::Active, MODE_ARRIVES),
        "a fresh paper core starts Active — the premise of both halves below"
    );

    let halt = run_trade_once(addr, settings.path(), &["halt", "--yes", "--reason", "e2e"], "");
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", halt.stdout, halt.stderr);
    assert!(halt.ok, "an ACCEPTED halt exits 0:\n{transcript}");
    assert!(halt.stdout.contains("connected: CONTROL (write)"), "{transcript}");
    assert!(halt.stdout.contains("accepted by the node"), "{transcript}");
    assert!(
        node_trading_state(&observer, WireTradingState::Halted, MODE_ARRIVES),
        "THE assertion: the node itself must be Halted, not merely told so\n{transcript}"
    );

    let resume = run_trade_once(addr, settings.path(), &["resume", "--yes"], "");
    let transcript =
        format!("--- stdout ---\n{}\n--- stderr ---\n{}", resume.stdout, resume.stderr);
    assert!(resume.ok, "an ACCEPTED resume exits 0:\n{transcript}");
    assert!(
        node_trading_state(&observer, WireTradingState::Active, MODE_ARRIVES),
        "a kill switch that cannot be released is a different failure\n{transcript}"
    );
}

/// **The confirm gate, proven by the node's mode rather than by the printed word.**
///
/// No `--yes`, and stdin closes with nothing on it: an EOF is a NO, the run exits non-zero, and the
/// node is STILL ACTIVE. The last clause is the one that matters — a build that sent the command
/// and then printed "aborted" would satisfy every other assertion here.
#[test]
fn a_one_shot_halt_with_a_closed_stdin_aborts_and_halts_nothing() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);
    let observer =
        RemoteCoreHandle::connect(addr, OBSERVE_KEY.as_bytes()).expect("observe connect");
    assert!(
        node_trading_state(&observer, WireTradingState::Active, MODE_ARRIVES),
        "premise: the node is Active"
    );

    let run = run_trade_once(addr, settings.path(), &["halt"], "");
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "an aborted confirm is a FAILURE — nothing was sent\n{transcript}");
    assert!(run.stdout.contains("aborted — nothing was sent"), "{transcript}");
    assert!(
        !run.stdout.contains("accepted by the node"),
        "nothing may have reached the node\n{transcript}"
    );
    // And the node agrees. `MODE_STAYS_AWAY` is a WINDOW, not a sleep: the mode is re-read until
    // the deadline, so a command that arrived late would still be caught here.
    assert!(
        !node_trading_state(&observer, WireTradingState::Halted, MODE_STAYS_AWAY),
        "the node must NOT be halted by a refused confirm\n{transcript}"
    );
}

/// **What a non-terminal stdin actually does — the claim the docs got wrong.**
///
/// The shipped `--help` and `docs/ops/kill-switches.md` both said a piped stdin reads as NO, which
/// would make an unattended `halt` without `--yes` a guaranteed no-op. It is a plain `read_line`:
/// a `y` on the pipe CONFIRMS, and the node halts. Both documents now say so, and this test is what
/// holds them honest — if a future change really does make a pipe a refusal, this fails, and the
/// words have to move back with it.
#[test]
fn a_piped_confirmation_halts_the_node_without_yes() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);
    let observer =
        RemoteCoreHandle::connect(addr, OBSERVE_KEY.as_bytes()).expect("observe connect");
    assert!(
        node_trading_state(&observer, WireTradingState::Active, MODE_ARRIVES),
        "premise: the node is Active"
    );

    let run = run_trade_once(addr, settings.path(), &["halt"], "y\n");
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(run.ok, "a confirmed halt exits 0:\n{transcript}");
    assert!(
        node_trading_state(&observer, WireTradingState::Halted, MODE_ARRIVES),
        "a `y` on a pipe CONFIRMS — a pipe is not a refusal\n{transcript}"
    );
}

/// The removed `state` verb, at the one-shot spelling: it must refuse AND name what replaced it.
/// A runbook line is what gets pasted at 3am, and `vike-cli trade state halted` was one.
#[test]
fn the_removed_state_verb_names_its_replacements_instead_of_being_an_unknown_argument() {
    let (_mount, addr) = spawn_node();
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    let run = run_trade_once(addr, settings.path(), &["state", "halted"], "");
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);
    assert!(!run.ok, "the removed verb must fail\n{transcript}");
    assert!(run.stderr.contains("REMOVED"), "{transcript}");
    assert!(run.stderr.contains("`halt`"), "it must name the WRITE that replaced it\n{transcript}");
    assert!(run.stderr.contains("`status`"), "…and the READ\n{transcript}");
}

/// The REPL line both settings cases send. The `policy.` key is a risk ceiling — the key class the
/// deleted retype was demanded for — so the case that used to need it most is the one pinned.
const SET_CEILING_LINE: &str = "set-setting policy.max_notional_per_order 250";

/// The ceiling's CURRENT value as the node renders it (`SettingsShow`'s effective row) — read the
/// same way the REPL reads it, so the assertion below is about the `old → new` line and not about
/// how a float prints.
fn ceiling_as_the_node_renders_it(addr: SocketAddr) -> String {
    let show = vike_tradehub_client::settings_show(addr, OBSERVE_KEY.as_bytes())
        .expect("the node serves its settings");
    show.rows
        .into_iter()
        .find(|r| r.key == "policy.max_notional_per_order")
        .expect("the node carries the ceiling row")
        .value
}

/// **A policy `set-setting` goes out after ONE `confirm? [y/N]`** (`docs/decisions/0086` point 7).
///
/// The session is `set-setting …`, `y`, `quit`. Against the retype build the SECOND prompt read
/// `quit` as the retyped key, called it a mismatch and aborted — so the node's row still held its
/// old value, which is the assertion that carries the weight: a printed line is not a write.
#[test]
fn a_policy_set_setting_goes_out_after_one_confirm() {
    let node_settings = node_settings_with_ceiling("100");
    let (_mount, addr) = spawn_node_with_settings(node_settings.path());
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);
    let old = ceiling_as_the_node_renders_it(addr);

    let run = run_trade_with(
        addr,
        settings.path(),
        &[],
        &[SET_CEILING_LINE, "y"],
        Liveness::ReadsEveryLine,
    );
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);

    assert!(run.ok, "the session must exit cleanly\n{transcript}");
    assert_eq!(
        stored_ceiling(node_settings.path()).as_deref(),
        Some("250"),
        "ONE y/N must be all a settings write takes — the node's row must hold the new value\n\
         {transcript}"
    );
    // The line the operator confirms AGAINST: the node's current value, then the new one — printed
    // with the preview, so it lands before the write's own result line.
    let change = format!("policy.max_notional_per_order: {old} → 250");
    let at = |needle: &str| run.stdout.lines().position(|l| l.contains(needle));
    let (Some(preview), Some(written)) = (at(&format!("change: {change}")), at("written:")) else {
        panic!("both the `change:` preview line and the `written:` result must print\n{transcript}")
    };
    assert!(preview < written, "the old → new line must come BEFORE the send\n{transcript}");
    assert!(
        run.stdout.contains("applies at the next restart"),
        "a policy key is never hot, so the node says restart — and the REPL must say so\n\
         {transcript}"
    );
    assert!(
        !run.stdout.to_ascii_lowercase().contains("retype"),
        "no retype, in any wording\n{transcript}"
    );
}

/// **`--yes` covers the settings write like every other write verb.** The retype used to be the one
/// prompt `--yes` could not answer, which made a policy write the only REPL line a script could not
/// complete: here the retype prompt read `quit` and the write never happened.
#[test]
fn a_policy_set_setting_under_yes_goes_out_with_no_prompt() {
    let node_settings = node_settings_with_ceiling("100");
    let (_mount, addr) = spawn_node_with_settings(node_settings.path());
    let settings = settings_dir_with_store(&[
        ("VIKE_TRADEHUB_OBSERVE_KEY", OBSERVE_KEY),
        ("VIKE_TRADEHUB_CONTROL_KEY", CONTROL_KEY),
    ]);

    let run = run_trade(addr, settings.path(), &[SET_CEILING_LINE], Liveness::ReadsEveryLine);
    let transcript = format!("--- stdout ---\n{}\n--- stderr ---\n{}", run.stdout, run.stderr);

    assert!(run.ok, "the session must exit cleanly\n{transcript}");
    assert_eq!(
        stored_ceiling(node_settings.path()).as_deref(),
        Some("250"),
        "under --yes the settings write needs no answer at all\n{transcript}"
    );
}

/// Pull `coid=<id>` out of the REPL's `preview:` line — the id the operator sees and types back.
fn coid_from_preview(stdout: &str) -> Option<String> {
    let line = stdout.lines().find(|l| l.starts_with("preview: SUBMIT"))?;
    let at = line.rfind("coid=")? + "coid=".len();
    Some(line[at..].split_whitespace().next()?.to_string())
}
