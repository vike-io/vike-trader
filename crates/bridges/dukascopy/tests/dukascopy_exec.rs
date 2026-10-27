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
        ledger_mount: None,
    }
}

fn order(coid: &str) -> OrderRequest {
    OrderRequest {
        data_delay: None,
        account: None,
        combo_legs: Vec::new(),
        client_order_id: coid.into(),
        venue: "dukascopy".into(),
        symbol: "EURUSD".into(),
        side: 1,
        qty: 1000.0,
        order_type: vike_model::OrderType::Market,
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
                    // Both open with the words they always had; each now says why and what happens next.
                    assert!(
                        e.reason.starts_with("bridge unavailable")
                            || e.reason.starts_with("bridge died"),
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
            assert!(e.reason.starts_with("bridge died"), "{}", e.reason);
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
// gap — nothing in production started this venue then, so nothing could leak past an engaged switch
// today; what it could do is have somebody wire the venue and inherit the silence with it.
//
// The sentinel path is INJECTED (`with_halt_path`), never resolved: `halt_path_from_env`
// memoizes in a `OnceLock`, so a test that engaged the process-wide one would engage it for every
// other test in this binary. Same idiom, and same reason, as
// `crates/vike-bridge-core/tests/exec_actor_halt.rs`.

/// A self-deleting directory plus a not-yet-existing sentinel path inside it.
///
/// ⚠ Returns the `ScratchDir` ALONGSIDE the path and the caller must BIND it: dropping the guard
/// removes the directory the sentinel lives in, out from under a client still consulting it. The
/// guard is the point — `crates/vike-ops/tests/hygiene/journal_scratch_gate.rs` refuses a bare
/// `env::temp_dir().join(...)` that creates something, because a hand-rolled cleanup line does not
/// run on the panic path and this repo has measured what that leaks. `ScratchDir::create_in` is one
/// of the three shapes that gate accepts, and it needs no new dependency here: `vike-model` is
/// layer 10 and already a normal dep of this crate.
fn halt_sentinel(tag: &str) -> (vike_paths::scratch::ScratchDir, std::path::PathBuf) {
    let dir = vike_paths::scratch::ScratchDir::create_in(
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
                vike_exec::halt::HALT_REJECT_REASON,
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
    // ⚠ The sidecar then FILLS the exit — the bare `Fill` and the `OrderFilled` wrap, the way the
    // real one dual-publishes — and does it ASYNCHRONOUSLY. Left on the channel, those two can be
    // queued ahead of phase 3's `OrderSubmitted` (which this client emits synchronously at submit),
    // and phase 3 then read the exit's fill as its answer: a CI flake, first seen 2026-09-28.
    // Reading them here makes the order of the channel a fact rather than a race.
    match recv_event(&mut rx) {
        Event::Fill(f) => assert_eq!(f.client_order_id, "exit"),
        other => panic!("expected the sidecar's Fill for the admitted exit, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderFilled(e) => assert_eq!(e.client_order_id, "exit"),
        other => panic!("expected the sidecar's OrderFilled for the admitted exit, got {other:?}"),
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
/// So the earlier draft's "one residual an SDK-staged start has to confirm" is CLOSED. What a real
/// start still adds is an end-to-end run against a live account, which is a different claim from
/// any link above and is what the `#[ignore]`d live smokes are for.
#[test]
fn the_sidecar_is_started_with_the_projects_jforex_home() {
    let dir =
        vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-duka-jforex-home")
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

// ---------------------------------------------------------------------------------------------
// Order modify: the Rust half of `IOrder.setOpenPrice` / `setRequestedAmount`
// ---------------------------------------------------------------------------------------------
//
// The fake answers the `modify` command the way the Java sidecar maps JForex's ORDER_CHANGED_OK /
// ORDER_CHANGED_REJECTED. What these prove is the Rust half: the command reaches the sidecar with the
// terms, the sidecar's answer comes back through the reader, and every refusal this side can make
// without asking is an explicit `OrderModifyRejected`. The JForex half (does `setOpenPrice` really
// move a resting EUR/USD limit on the demo account) is a live proof, not this file's.

fn resting_limit(coid: &str) -> OrderRequest {
    OrderRequest { order_type: vike_model::OrderType::Limit, price: Some(1.1), ..order(coid) }
}

/// A reduce-only LIMIT would OPEN an opposite position when hit (the sidecar nets only market orders), so it is
/// refused by name and never sent; it is a bracket's take-profit leg, which the core's preflight now refuses
/// before the entry (the row's `reduce_only_kinds` has no `limit`), so this is the backstop for a request that
/// did not pass that edge. The controls: the same limit without the flag and a reduce-only MARKET both
/// reach the sidecar, so the refusal is this one shape and not a client that refuses everything.
///
/// ⚠ Mutation proof: delete the refusal at the top of `DukascopyExecutionClient::submit` and the first half goes
/// red on the `OrderSubmitted` it then emits.
#[test]
fn a_reduce_only_limit_is_refused_by_name_and_never_sent() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready");

    client.submit(&OrderRequest { reduce_only: true, ..resting_limit("tp") });
    match recv_event(&mut rx) {
        Event::OrderRejected(r) => {
            assert_eq!(r.client_order_id, "tp");
            assert!(r.reason.contains("reduce-only LIMIT"), "{}", r.reason);
            assert!(r.reason.contains("NOT sent"), "{}", r.reason);
        }
        other => panic!("a reduce-only limit must be a terminal OrderRejected, got {other:?}"),
    }

    client.submit(&resting_limit("plain"));
    match recv_event(&mut rx) {
        Event::OrderSubmitted(e) => {
            assert_eq!(e.client_order_id, "plain", "nothing of `tp` was sent")
        }
        other => panic!("a plain limit must still reach the sidecar, got {other:?}"),
    }
    match recv_event(&mut rx) {
        Event::OrderAccepted(e) => assert_eq!(e.client_order_id, "plain"),
        other => panic!("expected the sidecar's OrderAccepted, got {other:?}"),
    }
    client.detach();
}

/// **The capability row's `reduce_only_kinds` is this client's refusal, executed.** Every kind the row accepts is sent as
/// a reduce-only order through the real client; its first event is the `OrderRejected` of the by-name refusal exactly
/// when the row does not list the kind (the limit), and the Rust-side `OrderSubmitted` of an order that reached the
/// sidecar otherwise (the market close and the venue's stop-loss). The core's preflight refuses a bracket's
/// take-profit from this row, so the two may not disagree.
#[test]
fn the_row_lists_exactly_the_reduce_only_kinds_the_client_sends() {
    let caps = vike_venues::caps_for("dukascopy");
    for kind in caps.supported_order_kinds {
        let (events, mut rx) = event_channel(64);
        let mut client =
            DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
                .expect("ready");
        client.submit(&OrderRequest {
            order_type: vike_model::OrderType::from_stored(kind),
            price: Some(1.1),
            trigger_price: Some(1.09),
            reduce_only: true,
            ..order("ro")
        });
        let refused = matches!(recv_event(&mut rx), Event::OrderRejected(_));
        assert_eq!(
            refused,
            !caps.reduce_only_kinds.contains(kind),
            "dukascopy/{kind}: the row and the client's refusal must agree"
        );
        client.detach();
    }
}

#[test]
fn a_modify_reaches_the_sidecar_and_its_order_modified_comes_back() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready");

    client.modify(&resting_limit("c1"), Some(2000.0), Some(1.2345));
    match recv_event(&mut rx) {
        Event::OrderModified(m) => {
            assert_eq!(m.client_order_id, "c1");
            assert_eq!(m.new_qty, Some(2000.0), "the new TOTAL quantity, in units");
            assert_eq!(m.new_price, Some(1.2345));
        }
        other => panic!("expected the sidecar's OrderModified, got {other:?}"),
    }

    // a price-only amend names no quantity
    client.modify(&resting_limit("c1"), None, Some(1.3));
    match recv_event(&mut rx) {
        Event::OrderModified(m) => {
            assert_eq!(m.new_qty, None);
            assert_eq!(m.new_price, Some(1.3));
        }
        other => panic!("expected OrderModified, got {other:?}"),
    }
    client.detach();
}

#[test]
fn a_modify_the_venue_refuses_is_an_order_modify_rejected_with_its_words() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["modify-reject".into()],
        &config(),
        events,
    )
    .expect("ready");

    client.modify(&resting_limit("c1"), None, Some(1.2));
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => {
            assert_eq!(r.client_order_id, "c1");
            assert_eq!(r.reason, "scripted refusal");
        }
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
    client.detach();
}

/// A halt blocks an amend, and says so. The control: with the sentinel gone the same amend goes through, so
/// the refusal is the sentinel's and not a client that refuses everything.
#[test]
fn a_modify_is_refused_under_an_engaged_sentinel_and_says_so() {
    let (_dir, sentinel) = halt_sentinel("modify");
    std::fs::write(&sentinel, b"").expect("engage the sentinel");
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready")
        .with_halt_path(sentinel.clone());

    client.modify(&resting_limit("c1"), Some(5000.0), None);
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => {
            assert_eq!(r.client_order_id, "c1");
            assert_eq!(r.reason, vike_exec::halt::HALT_REJECT_REASON);
        }
        other => panic!("a halted amend must be refused with the shared wording, got {other:?}"),
    }

    std::fs::remove_file(&sentinel).expect("disengage");
    client.modify(&resting_limit("c1"), Some(5000.0), None);
    match recv_event(&mut rx) {
        Event::OrderModified(m) => assert_eq!(m.new_qty, Some(5000.0)),
        other => panic!(
            "with the sentinel gone the SAME amend must reach the sidecar (otherwise the halted \
             half proves nothing), got {other:?}"
        ),
    }
    client.detach();
}

#[test]
fn an_amend_naming_nothing_to_change_is_refused() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(BRIDGE, &[], &config(), events)
        .expect("ready");
    client.modify(&resting_limit("c1"), None, None);
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => assert!(r.reason.contains("neither"), "{}", r.reason),
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
    client.detach();
}

/// A dead sidecar refuses the amend in words (non-terminal) instead of dropping it.
#[test]
fn a_modify_to_a_dead_sidecar_is_refused_not_dropped() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["ready-die".into()],
        &config(),
        events,
    )
    .expect("ready-die mode still handshakes ready");

    // One write can still slip into the pipe buffer before the child's exit is seen, so retry a little.
    let mut refused = false;
    'outer: for _ in 0..10 {
        client.modify(&resting_limit("c1"), None, Some(1.2));
        std::thread::sleep(Duration::from_millis(20));
        while let Some(ev) = try_recv_event(&mut rx) {
            if let Event::OrderModifyRejected(r) = ev {
                assert!(r.reason.contains("bridge unavailable"), "{}", r.reason);
                refused = true;
                break 'outer;
            }
        }
    }
    assert!(refused, "a dead sidecar never produced an OrderModifyRejected");
}

/// A sidecar jar built BEFORE the amend command announces no features in its READY line. Writing a `modify` to
/// it would be a command nobody answers (the old Java logs "unknown cmd skipped"), so the engine would wait for an
/// event that never comes: the client refuses it in words instead, naming the remedy.
#[test]
fn a_modify_for_a_jar_that_predates_the_amend_command_is_refused_in_words() {
    let (events, mut rx) = event_channel(64);
    let mut client = DukascopyExecutionClient::spawn_with_program(
        BRIDGE,
        &["old-jar".into()],
        &config(),
        events,
    )
    .expect("ready");
    client.modify(&resting_limit("c1"), None, Some(1.2));
    match recv_event(&mut rx) {
        Event::OrderModifyRejected(r) => {
            assert_eq!(r.client_order_id, "c1");
            assert!(r.reason.contains("predates the amend command"), "{}", r.reason);
        }
        other => panic!("expected OrderModifyRejected, got {other:?}"),
    }
    client.detach();
}

// ---- the stall cure: a sidecar that sends no ready is killed and started again ----------------

/// A per-attempt wait short enough to script a hung login in milliseconds.
const SHORT_WAIT: Duration = Duration::from_millis(400);

/// The pids a hang-mode fake wrote into its launch log, one per launch.
fn launches(log: &std::path::Path) -> Vec<u32> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| l.parse().expect("pid line"))
        .collect()
}

/// A hung first login is cut at the per-attempt wait, its process is REAPED before the next one
/// starts (so the login never has two JForex sessions), and the second start's `ready` is the client.
#[test]
fn a_hung_first_start_is_killed_and_the_respawn_becomes_the_client() {
    let dir = vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-duka-hang")
        .expect("scratch dir");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(16);
    let began = std::time::Instant::now();
    let mut client = DukascopyExecutionClient::spawn_with_attempts(
        BRIDGE,
        &["hang-once".into(), log.display().to_string()],
        &config(),
        events,
        SHORT_WAIT,
        3,
    )
    .expect("the respawn reached ready");
    let took = began.elapsed();
    assert!(took >= SHORT_WAIT, "the first start was not waited on: {took:?}");
    assert!(took < Duration::from_secs(20), "the respawn was not prompt: {took:?}");
    let pids = launches(&log);
    assert_eq!(pids.len(), 2, "one hung start, one respawn: {pids:?}");
    // `Child::wait` returned for the hung one: on Linux its /proc entry is gone, not a zombie.
    #[cfg(target_os = "linux")]
    assert!(!std::path::Path::new(&format!("/proc/{}", pids[0])).exists(), "hung sidecar left");
    client.detach();
}

/// A start whose first sidecar answers does NOT respawn anything: the launch log (pre-seeded so
/// `hang-once` sees a LATER launch and answers) still holds exactly the two lines afterwards.
#[test]
fn a_start_that_reaches_ready_is_never_respawned() {
    let dir = vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-duka-once")
        .expect("scratch dir");
    let log = dir.path().join("launches.txt");
    std::fs::write(&log, "1\n").expect("seed");
    let (events, _rx) = event_channel(16);
    let mut client = DukascopyExecutionClient::spawn_with_attempts(
        BRIDGE,
        &["hang-once".into(), log.display().to_string()],
        &config(),
        events,
        SHORT_WAIT,
        3,
    )
    .expect("ready");
    std::thread::sleep(SHORT_WAIT * 3);
    assert_eq!(launches(&log).len(), 2, "a ready start was started again");
    client.detach();
}

/// Every start hangs: exactly `max_attempts` sidecars are launched, each is gone when the call
/// returns, the call returns `Unavailable` (the existing give-up), and the total stays bounded.
#[test]
fn a_login_that_always_hangs_gives_up_after_the_attempts_and_leaves_no_sidecar() {
    let dir = vike_paths::scratch::ScratchDir::create_in(&std::env::temp_dir(), "vike-duka-all")
        .expect("scratch dir");
    let log = dir.path().join("launches.txt");
    let (events, _rx) = event_channel(16);
    let began = std::time::Instant::now();
    let err = DukascopyExecutionClient::spawn_with_attempts(
        BRIDGE,
        &["hang-always".into(), log.display().to_string()],
        &config(),
        events,
        SHORT_WAIT,
        3,
    )
    .err();
    let took = began.elapsed();
    assert_eq!(err, Some(vike_dukascopy::DukascopyError::Unavailable));
    assert!(took >= SHORT_WAIT * 3, "gave up before the third wait: {took:?}");
    assert!(took < Duration::from_secs(30), "the total is not bounded: {took:?}");
    let pids = launches(&log);
    assert_eq!(pids.len(), 3, "{pids:?}");
    #[cfg(target_os = "linux")]
    for pid in pids {
        assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists(), "sidecar {pid} left");
    }
}
