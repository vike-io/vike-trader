//! DukascopyExecutionClient integration tests against the scripted fake bridge
//! (src/bin/fake_jforex_bridge.rs) — no Java, no network. The fake bridge speaks the
//! real stdio protocol, so these tests cover: handshake (ready/fatal/silent-death),
//! the Rust-emitted OrderSubmitted, venue-derived Accepted/Filled/Canceled flowing
//! through the ingest lane, dead-child synthetic rejection, and shutdown/reap.

use std::time::Duration;

use vike_dukascopy::{DukascopyConfig, DukascopyExecutionClient};
use vike_exec::{ExecutionClient, Ingest, event_channel};
use vike_model::OrderRequest;
use vike_model::events::Event;

/// Path of the fake bridge binary (cargo builds crate bins for integration tests).
const BRIDGE: &str = env!("CARGO_BIN_EXE_fake_jforex_bridge");

fn config() -> DukascopyConfig {
    DukascopyConfig {
        login: "test-login".into(),
        password: "test-password".into(),
        server: String::new(),
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: "market".into(),
        price: None,
        trigger_price: None,
        reduce_only: false,
        time_in_force: Default::default(),
        gtd_expiry: None,
        ts: 1,
        parent_order_id: None,
        linked_order_ids: vec![],
        order_list_id: None,
        contingency_type: None,
        weight: 0.0,
        stop: None,
        trail: None,
        extreme: None,
        on_close: false,
        margin_mode: None,
        trigger_by: None,
    }
}

/// Receive the next Event from the ingest lane, failing loudly after 10s.
fn recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Event {
    let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let ingest = rt
        .block_on(async { tokio::time::timeout(Duration::from_secs(10), rx.recv()).await })
        .expect("timed out waiting for event")
        .expect("ingest channel closed");
    match ingest {
        Ingest::Event(ev) => ev,
        other => panic!("expected Ingest::Event, got {other:?}"),
    }
}

#[test]
fn ladder_flows_submitted_accepted_filled_canceled() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready");

    client.submit(&order("c1"));

    // Rust-side synchronous OrderSubmitted first…
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    // …then the venue-derived lifecycle from the (fake) sidecar.
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert!(e.venue_order_id.is_some());
        }
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
    // Dual-publish contract: the bare FillEvent (which the core Account folds into
    // position/PnL) arrives FIRST, then the OrderFilled wrap (which the FSM applies).
    match recv_event(&mut rx) {
        Event::Fill(fill) => {
            assert_eq!(fill.client_order_id, "c1");
            assert_eq!(fill.venue, "dukascopy"); // core routes on this exact string
            assert_eq!(fill.symbol, "EURUSD"); // canonical, not EUR/USD
            assert_eq!(fill.side, 1);
            assert_eq!(fill.last_qty, 1000.0); // units, not JForex millions
        }
        other => panic!("expected bare Fill first, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderFilled(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.fill.venue, "dukascopy");
            assert_eq!(e.fill.symbol, "EURUSD");
            assert_eq!(e.fill.side, 1);
            assert_eq!(e.fill.last_qty, 1000.0);
        }
        other => panic!("expected OrderFilled wrap, got {other:?}"),
    }

    client.cancel("c1");
    match recv_event(&mut rx) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderCanceled, got {other:?}"),
    }

    client.detach(); // shutdown + reap; must not hang or panic
}

#[test]
fn fatal_handshake_is_unavailable() {
    let (events, _rx) = event_channel(8);
    let err =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &["fatal".into()], &config(), events)
            .err()
            .expect("fatal handshake must fail spawn");
    assert_eq!(format!("{err:?}"), "Unavailable");
}

#[test]
fn silent_child_death_is_unavailable() {
    let (events, _rx) = event_channel(8);
    // "silent" exits without printing anything: reader hits EOF, handshake channel
    // disconnects, spawn returns Unavailable immediately (not after the 300s timeout).
    let err =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &["silent".into()], &config(), events)
            .err()
            .expect("silent death must fail spawn");
    assert_eq!(format!("{err:?}"), "Unavailable");
}

#[test]
fn dead_child_synthesizes_order_rejected() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["ready-die".into()],
        &config(),
        events,
    )
    .expect("ready-die mode still handshakes ready");

    // The child exits right after `ready`. A submit is terminated by EITHER path:
    // the failed stdin write ("bridge unavailable") or the reader's EOF drain of
    // in-flight coids ("bridge died"). One write can still slip into the pipe buffer
    // after the reader already drained, so keep a modest retry for scheduling slack.
    let mut saw_rejected = false;
    'outer: for i in 0..10 {
        client.submit(&order(&format!("c{i}")));
        std::thread::sleep(Duration::from_millis(20));
        // Drain whatever arrived; stop at the first OrderRejected.
        loop {
            match try_recv_event(&mut rx) {
                Some(Event::OrderRejected(e)) => {
                    assert!(
                        e.reason == "bridge unavailable" || e.reason == "bridge died",
                        "unexpected rejection reason: {}",
                        e.reason
                    );
                    saw_rejected = true;
                    break 'outer;
                }
                Some(_) => continue, // OrderSubmitted etc.
                None => break,
            }
        }
    }
    assert!(saw_rejected, "dead child never produced a synthetic OrderRejected");
}

#[test]
fn pre_ready_ghost_events_are_dropped() {
    let (events, mut rx) = event_channel(64);
    // "ghost" emits an event envelope BEFORE `ready` — a protocol violation the
    // reader must drop (never pump into ingest), while the handshake still succeeds.
    let mut client =
        DukascopyExecutionClient::spawn_with_program(BRIDGE, &["ghost".into()], &config(), events)
            .expect("ghost mode still handshakes ready");

    client.submit(&order("c1"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    // The very next event must be c1's Accepted — NOT the pre-ready ghost event.
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("ghost event leaked into ingest: {other:?}"),
    }
    client.detach();
}

#[test]
fn reader_eof_rejects_accepted_but_unterminated_order() {
    let (events, mut rx) = event_channel(64);
    // "accept-die": the submit is Accepted (non-terminal) and the child then dies —
    // the reader's EOF drain must synthesize the terminal rejection for it.
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["accept-die".into()],
        &config(),
        events,
    )
    .expect("accept-die mode still handshakes ready");

    client.submit(&order("c1"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderSubmitted, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, "c1"),
        other => panic!("expected OrderAccepted, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, "c1");
            assert_eq!(e.reason, "bridge died");
        }
        other => panic!("expected synthetic OrderRejected from EOF drain, got {other:?}"),
    }
    client.detach();
}

/// Non-blocking receive helper for the dead-child polling loop.
fn try_recv_event(rx: &mut tokio::sync::mpsc::Receiver<Ingest>) -> Option<Event> {
    match rx.try_recv() {
        Ok(Ingest::Event(ev)) => Some(ev),
        Ok(other) => panic!("expected Ingest::Event, got {other:?}"),
        Err(_) => None,
    }
}

// ---------------------------------------------------------------------------------------------
// The HALT sentinel on a BESPOKE client
// ---------------------------------------------------------------------------------------------
//
// This venue inherits nothing: it owns a child process rather than a `vike_bridge_core::ExecActor`,
// so the shared submit boundary every other venue's `submit` delegates to does not exist here and
// the sentinel had to be read by hand. `docs/ops/kill-switches.md` carried the absence as a LATENT
// gap — `vike_mount::make_engine` has no dukascopy live arm, so nothing could leak past an engaged
// switch today; what it could do is have somebody wire the arm and inherit the silence with it.
//
// The sentinel path is INJECTED (`with_halt_path`), never `VIKE_HALT_FILE`: `halt_path_from_env`
// memoizes in a `OnceLock`, so a test that engaged the process-wide one would engage it for every
// other test in this binary. Same idiom, and same reason, as
// `crates/vike-bridge-core/tests/exec_actor_halt.rs`.

/// A self-deleting directory plus a not-yet-existing sentinel path inside it.
///
/// ⚠ Returns the `ScratchDir` ALONGSIDE the path and the caller must BIND it: dropping the guard
/// removes the directory the sentinel lives in, out from under a client still consulting it. The
/// guard is the point — `crates/vike-ops/tests/journal_scratch_gate.rs` refuses a bare
/// `env::temp_dir().join(...)` that creates something, because a hand-rolled cleanup line does not
/// run on the panic path and this repo has measured what that leaks. `ScratchDir::create_in` is one
/// of the three shapes that gate accepts, and it needs no new dependency here: `vike-model` is
/// layer 10 and already a normal dep of this crate.
fn halt_sentinel(tag: &str) -> (vike_model::scratch::ScratchDir, std::path::PathBuf) {
    let dir = vike_model::scratch::ScratchDir::create_in(
        &std::env::temp_dir(),
        &format!("vike-duka-halt-{tag}"),
    )
    .expect("scratch dir for the sentinel");
    let path = dir.path().join("HALT");
    (dir, path)
}

/// `order`, with the caller-asserted reducing flag set — the ONE piece of evidence a boundary with
/// no position book has.
fn reducing(coid: &str) -> OrderRequest {
    OrderRequest { reduce_only: true, ..order(coid) }
}

/// THE regression test for this venue's half of kill-switch gap 3.
///
/// Three phases in ONE client, because each is the other's control:
///
///   1. engaged + OPENING → a terminal `OrderRejected` carrying the shared halt wording, and the
///      order never reaches the sidecar (no `OrderSubmitted`, which this client emits itself the
///      instant a submit is admitted — so its ABSENCE is the proof the refusal happened first);
///   2. still engaged + REDUCING → admitted, because a kill switch that traps an operator in a
///      position is the failure this whole exemption exists to prevent;
///   3. sentinel removed + the SAME opening order → admitted. Without this a client that had simply
///      stopped submitting anything at all would pass phase 1.
///
/// ⚠ Mutation proof: delete the `halt_engaged()` arm from `DukascopyExecutionClient::submit` and
/// phase 1 goes red on the `OrderSubmitted` it then emits.
#[test]
fn an_engaged_sentinel_refuses_an_opening_submit_and_still_admits_a_reducing_one() {
    let (_dir, sentinel) = halt_sentinel("submit");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");

    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready")
        .with_halt_path(sentinel.clone());

    // 1. OPENING, halted.
    client.submit(&order("opening"));
    match recv_event(&mut rx) {
        Event::OrderRejected(e) => {
            assert_eq!(e.client_order_id, "opening");
            assert_eq!(
                e.reason,
                vike_bridge_core::halt::HALT_REJECT_REASON,
                "the wording is SHARED with every other enforcing client — a GUI or a test that \
                 recognises a halt rejection recognises it by these exact bytes"
            );
        }
        other => panic!(
            "an opening submit under an engaged sentinel must be REFUSED terminally, and must not \
             reach the sidecar. Got: {other:?}"
        ),
    }

    // 2. REDUCING, still halted. `OrderSubmitted` is this client's own synchronous half, so seeing
    //    it at all means the request went past the boundary; `OrderAccepted` comes from the sidecar.
    client.submit(&reducing("exit"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "exit"),
        other => panic!(
            "a reduce_only submit must still go out under a halt — halting may never trap an \
             operator in a position. Got: {other:?}"
        ),
    }
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, "exit"),
        other => {
            panic!("expected the sidecar's OrderAccepted for the admitted exit, got {other:?}")
        }
    }

    // 3. Disengage: the SAME opening order now goes through.
    std::fs::remove_file(&sentinel).expect("disengage");
    client.submit(&order("opening-again"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => assert_eq!(e.client_order_id, "opening-again"),
        other => panic!(
            "with the sentinel gone the SAME opening order must go out — otherwise phase 1 proves \
             nothing about the halt. Got: {other:?}"
        ),
    }

    client.detach();
}

/// A CANCEL is never gated, on this client as on every other: a kill switch must let an operator
/// out. Its own test rather than a fourth phase above, because the property is unconditional —
/// there is no arm to add and this is what stops one being added.
#[test]
fn a_cancel_passes_straight_through_an_engaged_sentinel() {
    let (_dir, sentinel) = halt_sentinel("cancel");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");

    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready")
        .with_halt_path(sentinel.clone());

    client.cancel("resting");
    match recv_event(&mut rx) {
        Event::OrderCanceled(e) => assert_eq!(e.client_order_id, "resting"),
        other => panic!("a cancel is never halt-gated, got {other:?}"),
    }

    let _ = std::fs::remove_file(&sentinel);
    client.detach();
}

/// The fake bridge's report of the `user.home` it was given — `<user.home>/JForex/user-home.txt`.
///
/// Spelled here as well as in `crates/bridges/dukascopy/src/bin/fake_jforex_bridge.rs` because a
/// bin's constants are not importable from an integration test. Drift between the two makes the
/// test below FAIL (the file is looked for and not found), never pass quietly.
const JFOREX_FOLDER: &str = "JForex";
const USER_HOME_REPORT: &str = "user-home.txt";

/// **The sidecar is started with the PROJECT's JForex home** — the behavioural half of the
/// `-Duser.home=` fix, driven through a real child process.
///
/// # ⚠ What this proves, and what it does not
///
/// It PROVES: `DukascopyExecutionClient::spawn` delivers `-Duser.home=<home>` to a real spawned
/// process as one intact argv element, positioned inside the section a JVM reads its own options
/// from (before `-jar`), with the path surviving the trip through the operating system's argument
/// marshalling byte-for-byte — the stub writes back the exact string it received and this compares
/// it. A flag dropped, mis-spelled, truncated, re-quoted, or moved after `-jar` fails it.
///
/// It does NOT itself prove that a JVM honours `-Duser.home`, nor that the JForex SDK builds its
/// working directory from that property — the stub MODELS both (see its module doc). Read the name
/// as *started with*, not as *the SDK obeyed*.
///
/// ⚠ **Those two halves ARE established, off-test and by measurement, so do not read the paragraph
/// above as an open question.** Taken on the CI box 2026-09-23 against the JRE this project actually
/// runs (`bin/jre/jdk-17.0.19+10-jre`) and the jar it actually ships:
///
/// | link | evidence |
/// |---|---|
/// | the SDK builds the folder path from `user.home` | `com/dukascopy/login/utils/NetworkUtil` — the ONE class of 12,329 bundled `com/dukascopy/**` that reads it, carrying `defaultJForexFolderPath`, `getDefaultJForexFolderPath` and `"Cannot create the JForex directory."` |
/// | `-Duser.home` really moves it in THIS JRE | `java -XshowSettings:properties -version` reports `user.home = /home/the operator` bare and the substituted path with the flag |
/// | the folder is created WITH its parents | `com/dukascopy/login/utils/FilePathManager` calls `mkdirs` (not `mkdir`), so a missing `<project>/settings/state/jforex` is created rather than refused |
/// | the daemon may write there | `deploy/vike-tradehub.service`'s existing `ReadWritePaths` already grants `<project>/settings/state`, under `ProtectHome=yes` — no grant was widened for this fix |
///
/// So the earlier draft's "one residual an SDK-staged mount has to confirm" is CLOSED. What a real
/// mount still adds is an end-to-end run against a live account, which is a different claim from
/// any link above and is what the `#[ignore]`d live smokes are for.
#[test]
fn the_sidecar_is_started_with_the_projects_jforex_home() {
    let dir =
        vike_model::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-duka-jforex-home")
            .expect("scratch dir");
    // The project shape the production resolver produces: `<project>/settings/state/jforex`, with
    // the jar in the SIBLING tree so a home accidentally derived from the tool directory would
    // land somewhere this assertion does not look.
    let home = dir.path().join("settings").join("state").join("jforex");
    let jar = dir.path().join("bin").join("jforex").join("jforex-bridge.jar");
    std::fs::create_dir_all(jar.parent().expect("jar dir")).expect("bin tree");
    // `spawn`'s live gate is `is_file`, not a class-file read — an empty file is a jar as far as
    // this path is concerned, and the program we point `java` at is the stub, not a JVM.
    std::fs::write(&jar, b"").expect("plant the jar");

    let tools = vike_dukascopy::DukascopyTools {
        java: BRIDGE.to_string(),
        bridge_jar: jar,
        jforex_home: Some(home.clone()),
    };

    let (events, _rx) = event_channel(64);
    // Reaching `ready` at all means the stub parsed the JVM command line: it takes its MODE from
    // the application section, so a `-Duser.home=` it mistook for a mode would have selected no
    // known mode either way — the report below is what actually carries the proof.
    let mut client =
        DukascopyExecutionClient::spawn(config(), &tools, events).expect("the stub reached ready");

    let report = home.join(JFOREX_FOLDER).join(USER_HOME_REPORT);
    let got = std::fs::read_to_string(&report).unwrap_or_else(|e| {
        panic!(
            "the sidecar was started without a usable -Duser.home=: nothing wrote {} ({e}). Its \
             working directory would land in $HOME/JForex, which ProtectHome=yes makes \
             inaccessible to the daemon.",
            report.display()
        )
    });
    assert_eq!(
        got,
        home.display().to_string(),
        "the child received a DIFFERENT home from the one that was resolved"
    );

    client.detach();
}
