//! **The two ruling-16 client halves, driven as the SHIPPED binary against REAL peers** — the
//! test this pair shipped without, and the reason it had to exist.
//!
//! `report` and `study` shipped as complete clients of verbs no daemon served, so a REFUSAL was
//! their whole observable behaviour, and six prose sites (two module docs, a wire constant, the
//! `COMMANDS` table those skills tables are rendered from, and `crates/vike-cli/CLAUDE.md`) state
//! what that refusal says. ⚠ **All six have since been re-scoped** — the refusal is what an OLDER
//! daemon answers, not what every box does — so no site here claims the verb is unserved. That
//! sweep is what makes this suite the only thing left asserting reachability, and
//! [`report_against_a_node_that_serves_the_verb_surfaces_the_nodes_own_refusal`] carries what it
//! now asserts instead. ⚠ **`study`'s server arm HAS since landed** (stage 7:
//! `vike_datahub_client::proto`'s `Request::RunStudy`, served by
//! `crates/vike-backtest/src/compute_server.rs` when a study runner was mounted), so what this
//! suite drives for that half is no longer "the only behaviour there is" — it is the two refusals
//! that survive the arm: a peer that does not advertise the capability, and a box with nothing
//! listening. `report`'s half is still the original shape, its renderer unbuilt.
//! Every one of those claims was covered by a unit test over a PURE
//! formatter — `no_capability_lines(&full(), …)`, or an `io::Error` the test itself constructed
//! with the message it then asserted on. A pure formatter cannot answer the question those
//! sentences actually make: **can an invocation reach this path at all?** A refusal nothing
//! reaches formats perfectly and is still a promise the build does not keep.
//!
//! So each case here runs `CARGO_BIN_EXE_vike-cli` against something that really answers, and
//! asserts three things a formatter test cannot see: the words, the exit RUNG, and — where the
//! peer can count — that NOTHING went on the wire after the handshake.
//!
//! # What each test would have caught
//!
//! - [`study_against_a_real_server_of_this_protocol_takes_the_capability_refusal`] — that the
//!   capability refusal is REACHABLE. The peer is a real [`vike_datahub::serve`] over an in-memory
//!   store — same `Hello`/`Welcome`, same feature list, and it advertises no `study`. ⚠ This read
//!   *the compute daemon `study` is aimed at does not exist yet, and this is the only
//!   implementation of that protocol in the tree*, which stage 7 falsified on both counts. What
//!   the test needs is a peer that ANSWERS and does not advertise `study`, and the datahub is
//!   still exactly that — as is a compute daemon that mounted no study runner, the state a bare
//!   `cargo run -p vike-backtest --bin backtest` is in. The datahub is the cheaper double.
//! - [`study_sends_nothing_after_the_handshake`] — the negotiation's contract, against a peer that
//!   COUNTS frames. The real server would answer a stray frame either way, so proving "nothing was
//!   sent" needs a server that can report what it received. (`coverage_negotiation.rs`'s
//!   `spawn_counting_fake`, one crate over, is the precedent.)
//! - [`study_with_nothing_listening_still_names_the_command_that_works`] — the outcome on a box
//!   that has not stood the compute daemon up (every laptop, every fresh checkout): there is
//!   nothing to dial, so the run ends at the socket. It must still hand over `vike-backend study`,
//!   and it must exit on the RETRY rung (3) rather than the permanent one, because a socket that
//!   never answered is the one failure a wrapper may legitimately re-try. ⚠ This line called it
//!   THE ordinary outcome, which was true only while ruling 7's daemon did not exist; on a
//!   configured box the dial now succeeds and the capability refusal above is what fires.
//! - [`report_against_a_node_that_serves_the_verb_surfaces_the_nodes_own_refusal`] — the same
//!   question for the other half, against a REAL paper `vike-tradehub` node. ⚠ This bullet said its
//!   `served_features` *"genuinely omits `tearsheet`"* and described a DISCRIMINATION proof built on
//!   that: the client's sentence present, the server's absent. The capability shipped, so both halves
//!   inverted — the node serves the verb, the frame IS sent, and the reading is now that the
//!   SERVER's sentence must be present and the client's capability refusal absent. The two
//!   observations are the same two; only their polarity moved, and a node that silently stopped
//!   advertising `FEATURE_TEARSHEET` still fails on the same transcript. That test's own doc carries
//!   the whole argument and where the capability refusal is proven now.
//!
//! Loopback and in-memory only — no store on disk, no credentials, no venue, no external network.
//! The node keys are obviously-fake `DUMMY-*` strings in a throwaway settings directory, and every
//! child has the real key variables REMOVED from its environment so a developer box exporting them
//! cannot make a case pass vacuously.

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use vike_data::{HistStore, MemHistStore};
use vike_datahub_client::{PROTO_VERSION, Request, Response, read_frame, write_frame};
use vike_run::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::NodeKeys;
use vike_tradehub_client::wire::WireNodeIdentity;

/// Obviously fake: an HMAC secret, so any bytes work as long as both sides agree — and the
/// `DUMMY-` prefix makes it unmistakable in a diff or a process listing that no real credential is
/// involved.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-ruling-16-refusal-e2e";

/// The paper mount's symbol. Any string works — nothing resolves it against a venue.
const TOKEN: &str = "CLI_RULING16_REFUSAL_E2E_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors `tests/strategy_status_cli.rs`).
const RESOLUTION_TS: i64 = 3_000_000_000;

/// How long a scripted peer gets to report what it observed. Generous: these threads do a
/// handshake and a read, and the value bounds a HANG rather than a slow machine.
const REPORT_TIMEOUT: Duration = Duration::from_secs(20);

// ─── fixtures ────────────────────────────────────────────────────────────────────────────────

/// A throwaway `<project>/settings` holding a NODE-KEY store with (only) the observe key — the
/// daemon's own documented configuration, and the resolution path `report` uses. `study` resolves
/// the DATAHUB pair, which this store deliberately does not carry: a key-less peer answers an
/// unauthenticated dial, and that is the configuration under test.
fn settings_dir_with_observe_key() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(vike_secrets::NODE_FILE);
    std::fs::write(&path, format!("VIKE_TRADEHUB_OBSERVE_KEY={OBSERVE_KEY}\n"))
        .expect("write node-key store");
    // 0600 like a real store, so the permission finding does not bury the assertions' transcripts.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod 600");
    }
    dir
}

/// A recipe file on THIS machine. Its CONTENT is irrelevant to every case here — no peer parses
/// it, and `study` reads it only to fail a path typo before opening a socket — but its SIZE is
/// reported in the refusal, which is what tells a reader which half of the command failed.
fn recipe_file(dir: &Path) -> std::path::PathBuf {
    let path = dir.join("recipe.toml");
    std::fs::write(&path, "[learner]\nkind = \"lightgbm\"\n").expect("write recipe");
    path
}

fn run_cli(settings_dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vike-cli"))
        .args(args)
        .env("VIKE_SETTINGS_DIR", settings_dir)
        // The configuration under test is "no key exported"; a developer box that exports one
        // would otherwise change which constructor the verb reaches.
        .env_remove("VIKE_DATAHUB_OBSERVE_KEY")
        .env_remove("VIKE_DATAHUB_CONTROL_KEY")
        .env_remove("VIKE_TRADEHUB_OBSERVE_KEY")
        .env_remove("VIKE_TRADEHUB_CONTROL_KEY")
        // The two REMOVED risk variables refuse startup when set — cleared so an operator box's
        // stale export cannot fail an unrelated verb's test on the startup refusal.
        .env_remove("VIKE_MAX_ORDER_NOTIONAL")
        .env_remove("VIKE_TRADEHUB_MAX_ORDER_NOTIONAL")
        .output()
        .unwrap_or_else(|e| panic!("run vike-cli {args:?}: {e}"))
}

fn transcript(out: &Output) -> String {
    format!(
        "--- exit {:?} ---\n--- stdout ---\n{}\n--- stderr ---\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// Bind an ephemeral loopback listener and spawn the REAL datahub `serve` over an in-memory store.
/// `MemHistStore` answers nothing interesting, which is the point: these cases end at the
/// handshake and never send a verb.
fn spawn_real_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistStore + Send + Sync> = Arc::new(MemHistStore::new());
    thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    addr
}

/// A key-less peer that completes the version handshake and then COUNTS every frame the client
/// sends until EOF. Hand-rolled rather than driven through the real `serve` for the reason
/// `coverage_negotiation.rs` gives one crate over: proving "nothing was sent" needs a server that
/// can report what arrived, and the real one would answer either way.
fn spawn_counting_datahub() -> (SocketAddr, mpsc::Receiver<Result<usize, String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let verdict = (|| -> Result<usize, String> {
            let (mut stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Hello { .. }) => {}
                other => return Err(format!("expected Hello, got {other:?}")),
            }
            // A key-less datahub: no nonce, and a feature list that names everything this peer
            // serves EXCEPT `study` — which is every peer that speaks this protocol today.
            write_frame(
                &mut stream,
                &Response::Welcome {
                    proto_version: PROTO_VERSION,
                    features: vec!["backtest".to_string(), "inventory".to_string()],
                    nonce: None,
                },
            )
            .map_err(|e| format!("write Welcome: {e}"))?;
            let mut frames_after_hello = 0usize;
            while read_frame::<_, Request>(&mut stream).is_ok() {
                frames_after_hello += 1;
            }
            Ok(frames_after_hello)
        })();
        let _ = tx.send(verdict);
    });
    (addr, rx)
}

/// A REAL paper `vike-tradehub` node on an ephemeral loopback port, observe scope only — the
/// `tests/strategy_status_cli.rs` harness, trimmed to what a tearsheet refusal needs (no control
/// key, no command sink, one mount). The returned mount keeps the core alive.
fn spawn_real_node() -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::polymarket(TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let identity = WireNodeIdentity {
        name: "e2e-hub".to_string(),
        strategy: "spread_maker".to_string(),
        params: "spread=0.01".to_string(),
        live: false,
        build: "vike-tradehub test-build".to_string(),
        advertise_addr: String::new(),
    };
    let publisher =
        publish::spawn_with_mounts(mount.handle.snapshot_cell(), Some(identity), Vec::new());
    let keys = NodeKeys::new(OBSERVE_KEY.as_bytes().to_vec(), Vec::new());
    thread::spawn(move || {
        let _ = server::serve(
            listener,
            publisher,
            keys,
            None,
            server::ControlLimitsConfig::default(),
            None,
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            // No REQ-2 datahub advertisement — this suite exercises a node READ refusal only.
            None,
        );
    });
    (mount, addr)
}

// ─── study ───────────────────────────────────────────────────────────────────────────────────

/// **The capability refusal is REACHABLE**, against a real server of this protocol.
///
/// ⚠ The peer is a `vike-datahub` rather than the compute daemon, and that is not a cheat — but the
/// reason CHANGED in stage 7 and the old one is worth recording, because it was *ruling 7's
/// `vike-backend backtest --addr` does not exist yet*, which is now false:
/// `crates/vike-backtest/src/compute_server.rs` IS that daemon and `deploy/vike-backtest.service`
/// ships it. What this test needs is not "the only implementation" — it is A PEER THAT ANSWERS THE
/// HANDSHAKE AND DOES NOT ADVERTISE `study`, and there are two real ones: a datahub, which serves a
/// different plane entirely, and a compute daemon that MOUNTED no study runner
/// (`vike_backtest::compute_server`'s `StudyRunFactory` left `None`). The datahub is the cheaper
/// double and speaks the identical protocol — the same `Hello`/`Welcome`, the same
/// `Welcome.features` list, and no `study` in it — so what this proves is exactly what the prose
/// claims: reach a peer that answers, and the client names the missing capability, says nothing was
/// sent, and hands over the invocation that works.
#[test]
fn study_against_a_real_server_of_this_protocol_takes_the_capability_refusal() {
    let settings = settings_dir_with_observe_key();
    let recipe = recipe_file(settings.path());
    let addr = spawn_real_datahub();

    let out = run_cli(
        settings.path(),
        &[
            "research",
            "study",
            "--study",
            "cohort",
            "--recipe",
            recipe.to_str().expect("utf-8 recipe path"),
            "--from",
            "2026-04-07T05",
            "--to",
            "2026-08-05T05",
            "--addr",
            &addr.to_string(),
        ],
    );
    let t = transcript(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The peer ANSWERED, so this is the permanent rung — never the retry one.
    assert_eq!(out.status.code(), Some(1), "a peer that ANSWERED is not the connect rung:\n{t}");
    // ⚠ The QUOTED capability string, not the bare word: `vike-cli research study:` prefixes every
    // line this verb writes, so `contains("study")` would be answered by the prefix and would stay
    // true if the negotiation were deleted outright.
    assert!(stderr.contains("\"study\" capability"), "it names the missing capability:\n{t}");
    assert!(stderr.contains("nothing was sent"), "{t}");
    assert!(stderr.contains("bytes"), "the recipe read is reported, so a reader knows:\n{t}");
    assert!(stderr.contains("vike-backend study"), "the command that works is named:\n{t}");
    assert!(stderr.contains("--from 2026-04-07T05"), "this run's own window is carried:\n{t}");
}

/// …and the contract that makes the refusal worth having: NOTHING follows the handshake. A frame
/// sent to a peer that does not serve the verb is an undecodable frame and an opaque error, which
/// is the outcome negotiating exists to avoid.
#[test]
fn study_sends_nothing_after_the_handshake() {
    let settings = settings_dir_with_observe_key();
    let recipe = recipe_file(settings.path());
    let (addr, rx) = spawn_counting_datahub();

    let out = run_cli(
        settings.path(),
        &[
            "research",
            "study",
            "--study=cohort",
            &format!("--recipe={}", recipe.to_str().expect("utf-8 recipe path")),
            "--from=1785906000",
            "--to=1788584400",
            &format!("--addr={addr}"),
        ],
    );
    let t = transcript(&out);
    assert_eq!(out.status.code(), Some(1), "{t}");

    let frames = rx
        .recv_timeout(REPORT_TIMEOUT)
        .expect("the counting peer reported")
        .unwrap_or_else(|e| panic!("the counting peer desynced: {e}\n{t}"));
    assert_eq!(frames, 0, "a frame reached a peer that does not serve the verb:\n{t}");
}

/// The ORDINARY outcome on a box today — nothing is listening on the compute address, because the
/// daemon that binds it has not shipped. The run ends at the socket, and the two things it owes
/// the operator are both asserted: the command that works, and the RETRY rung, because a socket
/// that never answered is the one failure a wrapper may legitimately re-try.
///
/// The address is a port that was bound and then RELEASED, so it is refused rather than
/// black-holed: a firewalled address would spend the client's whole connect timeout here.
#[test]
fn study_with_nothing_listening_still_names_the_command_that_works() {
    let settings = settings_dir_with_observe_key();
    let recipe = recipe_file(settings.path());
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
        listener.local_addr().expect("resolve assigned port")
    };

    let out = run_cli(
        settings.path(),
        &[
            "research",
            "study",
            "--study=cohort",
            &format!("--recipe={}", recipe.to_str().expect("utf-8 recipe path")),
            "--from=a",
            "--to=b",
            &format!("--addr={dead}"),
        ],
    );
    let t = transcript(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);

    assert_eq!(out.status.code(), Some(3), "a socket that never answered is the retry rung:\n{t}");
    assert!(stderr.contains("cannot connect"), "{t}");
    assert!(stderr.contains("no compute daemon is listening there"), "it says WHY:\n{t}");
    assert!(stderr.contains("vike-backend study"), "the command that works is named:\n{t}");
}

// ─── report ──────────────────────────────────────────────────────────────────────────────────

/// **The live report verb against a REAL `vike-tradehub` node, now that the node SERVES it.**
///
/// ⚠ This test was `report_against_a_real_node_takes_the_capability_refusal_and_sends_nothing`,
/// and its premise was that *"that daemon's `served_features` genuinely omits `tearsheet`, which
/// is the state this verb was written for"*. That state is gone: the arm and the capability string
/// shipped together (`crates/vike-tradehub/src/server.rs`'s
/// `the_tearsheet_capability_is_advertised_now_that_the_arm_serves_it` holds the two equal), so
/// the client's negotiation PASSES and the frame is sent — which is the feature working, not a
/// regression. The old assertions could only be restored by un-shipping the capability.
///
/// # What is proven here instead, and it is not weaker
///
/// The interesting failure was never "the capability is missing" — it is a node that CAN serve the
/// verb and still cannot answer, which is the ordinary state of a freshly-started node. This
/// harness produces exactly that: `spawn_real_node` stands one up with no settings source, so the
/// daemon can serve `Request::Tearsheet` and cannot resolve its own journal directory. What must
/// hold then is that the NODE's own sentence reaches the operator verbatim, because the alternative
/// — a fabricated empty tearsheet — reports a flat account to somebody whose account is not flat.
///
/// It also keeps the discrimination that made the old test work, pointing the other way: the
/// SERVER's words being PRESENT is now the evidence the frame was sent, and the client's
/// capability sentence being ABSENT is the evidence it did not refuse locally. So a regression that
/// silently dropped `FEATURE_TEARSHEET` from `served_features` fails this test, in the same
/// transcript, by the same two observations read in reverse.
///
/// # Where the capability refusal is proven now
///
/// `crates/vike-cli/src/cmd/report.rs`'s own unit tests construct `failure_lines`' sentence
/// directly — the branch is live code for an OLDER daemon and is covered there. It is not covered
/// end-to-end any more and deliberately so: no node this tree builds can produce the state, and
/// scripting a fake tradehub peer through the HMAC observe handshake to assert a string a unit test
/// already asserts would buy nothing. The study half's twin
/// ([`study_against_a_real_server_of_this_protocol_takes_the_capability_refusal`] and
/// [`study_sends_nothing_after_the_handshake`]) still exercises the negotiation end-to-end on the
/// shared code path, against a daemon that genuinely omits ITS verb.
#[test]
fn report_against_a_node_that_serves_the_verb_surfaces_the_nodes_own_refusal() {
    let settings = settings_dir_with_observe_key();
    let (_mount, addr) = spawn_real_node();

    let out = run_cli(
        settings.path(),
        &["report", "--node", &addr.to_string(), "--seed", "25000", "--json"],
    );
    let t = transcript(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);

    // The node ANSWERED (it completed the observe handshake), so this is the permanent rung.
    assert_eq!(out.status.code(), Some(1), "a node that ANSWERED is not the connect rung:\n{t}");
    // The frame WAS sent, and the server's own words are on the transcript — the inverse of what
    // this test used to assert, and the proof the capability is advertised.
    assert!(
        stderr.contains("tearsheet unavailable"),
        "the node's own refusal must reach the operator verbatim:\n{t}"
    );
    assert!(
        stderr.contains("without a settings source"),
        "…including WHY, so it can be fixed — this node cannot resolve its journal:\n{t}"
    );
    // ⚠ The QUOTED capability string, not the bare word: this verb prefixes every line it writes,
    // so a bare `contains("tearsheet")` is answered by the sentence above.
    assert!(
        !stderr.contains("\"tearsheet\" capability"),
        "the CLIENT's capability refusal is on this transcript — the node stopped advertising \
         `FEATURE_TEARSHEET`, or the negotiation regressed:\n{t}"
    );
    // stdout is a machine surface: a refusal must leave it empty rather than print half a document.
    assert!(out.stdout.is_empty(), "nothing may reach stdout on a refusal:\n{t}");
}
