//! **The two ruling-16 client halves, driven as the SHIPPED binary against REAL peers** — the
//! test this pair shipped without, and the reason it had to exist.
//!
//! `report` and `study` shipped as complete clients of verbs no daemon served, so a REFUSAL was
//! their whole observable behaviour, and six prose sites (two module docs, a wire constant, the
//! `COMMANDS` table those skills tables are rendered from, and `crates/vike-cli/CLAUDE.md`) state
//! what that refusal says. ⚠ **All six have since been re-scoped** — the refusal is what an OLDER
//! daemon answers, not what every box does — so no site here claims the verb is unserved. That
//! sweep is what makes this suite the only thing left asserting reachability. ⚠ **`study`'s server
//! arm HAS since landed** (stage 7:
//! `vike_backtesthub_client::proto`'s `Request::RunStudy`, served by
//! `crates/vike-backtesthub/src/backtesthub_server.rs` when a study runner was mounted), so what this
//! suite drives for that half is no longer "the only behaviour there is" — it is the two refusals
//! that survive the arm: a peer that does not advertise the capability, and a box with nothing
//! listening. ⚠ **`report`'s live half is RETIRED** (owner, 2026-11-18): `--node` and a bare run
//! selector are refused at the parse, and
//! [`report_refuses_the_retired_spellings_and_dials_nothing`] is what this suite asserts of it now.
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
//! - [`study_against_the_data_daemon_is_told_which_daemon_answered`] — that a REAL peer of the
//!   other plane is named rather than misread. The peer is a real [`vike_datahub::serve`] over an
//!   in-memory store. ⚠ It was this suite's capability-refusal peer while both daemons spoke one
//!   protocol; since backtesthub protocol 9 (the datahub stays at 8) the client names the DATA
//!   daemon at the handshake, sends nothing after the Hello, and still hands over
//!   `vike-backend study`.
//! - [`study_sends_nothing_after_the_handshake`] — that the capability refusal is REACHABLE, and
//!   the negotiation's contract, against a peer that answers at the backtesthub's protocol,
//!   advertises no `study` and COUNTS frames. Every real backtesthub advertises `study` now, and a
//!   real server would answer a stray frame either way, so proving "nothing was sent" needs a
//!   server that can report what it received.
//!   (`crates/vike-datahub-client/src/test_support.rs`'s `spawn_fake_peer`, one crate over,
//!   is the precedent.)
//! - [`study_with_nothing_listening_still_names_the_command_that_works`] — the outcome on a box
//!   that has not stood the backtesthub up (every laptop, every fresh checkout): there is
//!   nothing to dial, so the run ends at the socket. It must still hand over `vike-backend study`,
//!   and it must exit on the RETRY rung (3) rather than the permanent one, because a socket that
//!   never answered is the one failure a wrapper may legitimately re-try. ⚠ This line called it
//!   THE ordinary outcome, which was true only while ruling 7's daemon did not exist; on a
//!   configured box the dial now succeeds and the capability refusal above is what fires.
//! - [`report_refuses_the_retired_spellings_and_dials_nothing`] — the other half, retired: the
//!   words, the usage rung, an empty stdout, and that `--node` never connects to the address it
//!   names. It replaced a test that drove `--node` against a REAL paper `vike-tradehub` node and read
//!   that node's own refusal off the transcript; with the door retired there is no frame to send.
//!
//! Loopback and in-memory only — no store on disk, no credentials, no venue, no external network.
//! The device key is made from an obviously-fake `DUMMY-*` value in a throwaway settings directory, and
//! every child has the retired key variables REMOVED from its environment, so a developer box still
//! exporting one is not refused at startup instead of reaching the case.

use crate::common;

use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use vike_data::{HistoryStore, HistoryStoreMemory};
use vike_datahub_client::proto::Request;
use vike_hub_protocol::frame::{read_frame, write_frame};
use vike_hub_protocol::handshake::{HandshakeFrame, HandshakeReply};

/// Obviously fake: a fixture VALUE naming a device (`common::device_for`) — and the `DUMMY-` prefix
/// makes it unmistakable in a diff or a process listing that no real credential is involved.
const OBSERVE_KEY: &str = "DUMMY-observe-key-for-the-ruling-16-refusal-e2e";

/// How long a scripted peer gets to report what it observed. Generous: these threads do a
/// handshake and a read, and the value bounds a HANG rather than a slow machine.
const REPORT_TIMEOUT: Duration = Duration::from_secs(20);

// ─── fixtures ────────────────────────────────────────────────────────────────────────────────

/// A throwaway `<project>/settings` whose settings database holds a device key (an observe-role
/// value), the one key every dial of this binary signs with. The scripted peers are key-less (no
/// nonce): they answer an unauthenticated dial, and that is the configuration under test.
fn settings_dir_with_observe_key() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    common::seed_device(dir.path(), OBSERVE_KEY);
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
        // The retired key variables refuse startup when set (`vike_config::REMOVED_ENV`): a developer
        // box still exporting one must not fail the case on that refusal.
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
/// `HistoryStoreMemory` answers nothing interesting, which is the point: these cases end at the
/// handshake and never send a verb.
fn spawn_real_datahub() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(HistoryStoreMemory::new());
    thread::spawn(move || {
        let _ = vike_datahub::serve(listener, store);
    });
    addr
}

/// A key-less peer that completes the version handshake at the BACKTESTHUB's protocol (the one
/// `research study` speaks) and then COUNTS every frame the client sends until EOF. Hand-rolled
/// rather than driven through a real `serve` for the reason `coverage_negotiation.rs` gives one
/// crate over: proving "nothing was sent" needs a server that can report what arrived, and the real
/// one would answer either way. It is also the one peer left that answers this protocol without
/// `study`: every real backtesthub advertises it, and since protocol 9 the datahub speaks another.
fn spawn_counting_backtesthub() -> (SocketAddr, mpsc::Receiver<Result<usize, String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let verdict = (|| -> Result<usize, String> {
            let (mut stream, _) = listener.accept().map_err(|e| format!("accept: {e}"))?;
            match read_frame::<_, HandshakeFrame>(&mut stream) {
                Ok(HandshakeFrame::Hello { .. }) => {}
                other => return Err(format!("expected Hello, got {other:?}")),
            }
            // A key-less peer: no nonce, and a feature list that names no `study` (the frame shape
            // is the one hub `Welcome`, `vike_hub_protocol::handshake`'s).
            write_frame(
                &mut stream,
                &HandshakeReply::Welcome {
                    proto_version: vike_backtesthub_client::proto::PROTO_VERSION,
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

// ─── study ───────────────────────────────────────────────────────────────────────────────────

/// **A real peer of the OTHER plane is named, not misread**: the client meets a real datahub.
///
/// ⚠ Until backtesthub protocol 9 the two daemons spoke one protocol, so this test read the
/// datahub as the cheap double of A PEER THAT ANSWERS THE HANDSHAKE AND DOES NOT ADVERTISE `study`
/// and asserted the capability refusal against it. Since the bump (the datahub stays at 8) the
/// client identifies the DATA daemon from its `Welcome` before it compares the versions
/// (`vike_hub_protocol::conn::WrongDaemon`), so what a real datahub now proves is that answer: the
/// permanent rung, the daemon named, nothing sent after the Hello, and the invocation that works
/// still handed over. Every real backtesthub advertises `study`, so the capability refusal is
/// asserted against the scripted peer of [`study_sends_nothing_after_the_handshake`].
#[test]
fn study_against_the_data_daemon_is_told_which_daemon_answered() {
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
    assert!(stderr.contains("is the DATA daemon"), "it names the daemon that answered:\n{t}");
    assert!(stderr.contains("Nothing was sent after the Hello"), "{t}");
    assert!(stderr.contains("vike-backend study"), "the command that works is named:\n{t}");
    assert!(stderr.contains("--from 2026-04-07T05"), "this run's own window is carried:\n{t}");
}

/// **The capability refusal is REACHABLE**, and the contract that makes it worth having: NOTHING
/// follows the handshake. A frame sent to a peer that does not serve the verb is an undecodable
/// frame and an opaque error, which is the outcome negotiating exists to avoid. The peer answers at
/// the backtesthub's own protocol and advertises no `study` (the one such peer left, see
/// [`spawn_counting_backtesthub`]).
#[test]
fn study_sends_nothing_after_the_handshake() {
    let settings = settings_dir_with_observe_key();
    let recipe = recipe_file(settings.path());
    let (addr, rx) = spawn_counting_backtesthub();

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
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "a peer that ANSWERED is not the connect rung:\n{t}");
    // ⚠ The QUOTED capability string, not the bare word: `vike-cli research study:` prefixes every
    // line this verb writes, so `contains("study")` would be answered by the prefix and would stay
    // true if the negotiation were deleted outright.
    assert!(stderr.contains("\"study\" capability"), "it names the missing capability:\n{t}");
    assert!(stderr.contains("nothing was sent"), "{t}");
    assert!(stderr.contains("bytes"), "the recipe read is reported, so a reader knows:\n{t}");
    assert!(stderr.contains("vike-backend study"), "the command that works is named:\n{t}");

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
/// The address is loopback port 1, so it is refused rather than black-holed (a firewalled address
/// would spend the client's whole connect timeout here), and no test's listener can be handed it, as
/// a port bound and released can be.
#[test]
fn study_with_nothing_listening_still_names_the_command_that_works() {
    let settings = settings_dir_with_observe_key();
    let recipe = recipe_file(settings.path());
    let dead = "127.0.0.1:1";

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
    assert!(stderr.contains("no backtesthub is listening there"), "it says WHY:\n{t}");
    assert!(stderr.contains("vike-backend study"), "the command that works is named:\n{t}");
}

// ─── report ──────────────────────────────────────────────────────────────────────────────────

/// **The two retired spellings, as the shipped binary answers them** (owner, 2026-11-18): the
/// live-journal `--node` and a bare run selector are each refused on the USAGE rung with ONE line
/// naming the spelling that replaced it, stdout stays empty, and `--node` DIALS NOTHING — the
/// listener it names is never connected to, because the refusal is a parse error that runs before
/// any key is read. A device key IS in the store, so a dial was possible: its absence is the
/// refusal's doing, not the box's.
#[test]
fn report_refuses_the_retired_spellings_and_dials_nothing() {
    let settings = settings_dir_with_observe_key();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    listener.set_nonblocking(true).expect("a non-blocking listener");
    let addr = listener.local_addr().expect("resolve assigned port");

    let out = run_cli(
        settings.path(),
        &["report", "--node", &addr.to_string(), "--cash", "25000", "--json"],
    );
    let t = transcript(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "a retired spelling is the usage rung:\n{t}");
    let line = format!("vike-cli report: {}", vike_cli::cmd::report::JOURNAL_DOORS_RETIRED);
    assert!(stderr.lines().any(|l| l == line), "the one shared line, whole:\n{t}");
    assert!(stderr.contains("report account:<venue>/<LABEL>"), "it names the new spelling:\n{t}");
    assert!(out.stdout.is_empty(), "nothing may reach stdout on a refusal:\n{t}");
    match listener.accept() {
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
        other => panic!("`--node` connected to the address it named: {other:?}\n{t}"),
    }

    let out = run_cli(settings.path(), &["report", "@last", "--json"]);
    let t = transcript(&out);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "a retired spelling is the usage rung:\n{t}");
    assert!(
        stderr.lines().any(|l| l
            == "vike-cli report: a bare run selector is retired: spell it `vike-cli report run:@last`"),
        "the operator's own selector, in the spelling that works:\n{t}"
    );
    assert!(out.stdout.is_empty(), "nothing may reach stdout on a refusal:\n{t}");
}
