use super::*;
use std::net::{SocketAddr, TcpListener};
use std::thread;

use crate::auth;
use crate::proto::{NODE_PROTO_VERSION, read_frame, write_frame};
use crate::wire::WireOrderRequest;

const KEY: &[u8] = b"control-key-for-the-never-sent-tests";
const WAIT: Duration = Duration::from_secs(5);
/// The scripted reply deadline the silent-death test drives through
/// [`RemoteControlHandle::connect_with_reply_timeout`]. Production is
/// [`CONTROL_REPLY_TIMEOUT`] (30 s); the property is the same at any scale.
const TEST_REPLY_TIMEOUT: Duration = Duration::from_millis(300);
/// How long a silent scripted node holds its socket open saying nothing — far past every
/// assertion, so the test can never be passing because the node finally hung up.
const SILENT_HOLD: Duration = Duration::from_secs(30);
/// How long to let a loopback FIN arrive before offering the command the whole never-sent story
/// turns on. Generous by three orders of magnitude — a `close()` on `127.0.0.1` is delivered in
/// microseconds — because a test that raced the FIN would be testing the wrong thing entirely:
/// the point is a link the worker finds ALREADY dead, not one dying under it.
const FIN_SETTLE: Duration = Duration::from_millis(100);
/// How long [`the_start_gate_holds_the_worker_until_it_is_released`] looks for a verdict that
/// must not exist. Long enough that an UNGATED worker — which resolves a dead-link command in
/// microseconds — would certainly have produced one, and paid only by that one test.
const GATE_HELD_PROBE: Duration = Duration::from_millis(200);
/// How many times
/// [`a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected`] replays the
/// scenario. Sized by what it costs rather than by a confidence target: each round is one
/// loopback connection plus [`FIN_SETTLE`], so this is a few seconds — and the rounds are
/// evidence only on the BROKEN ordering. Once the store precedes the publish, one round proves
/// as much as a thousand, and these exist to keep the ordering from silently coming back.
const ORDERING_ROUNDS: usize = 30;

/// When the scripted node hangs up.
#[derive(Clone, Copy)]
enum Script {
    /// Close CLEANLY straight after `AuthOk`, having read no command — the node's own idle
    /// close, a daemon restart, a tunnel dropping one link. The client's next command meets a
    /// socket with a FIN already in its receive queue.
    CloseAfterAuth,
    /// Read ONE command and then close WITHOUT answering — the command reached the node and
    /// its verdict was lost on the way back. Whether it executed is genuinely unknowable from
    /// here, which is the point.
    ReadOneCommandThenClose,
    /// Read ONE command and then go SILENT while HOLDING the socket open — no answer, and no
    /// FIN or RST either. The silent death: a laptop sleeping, a Wi-Fi change, a VPN re-key
    /// under an `ssh -L` whose forwarded socket stays open locally. Distinct from
    /// [`Script::ReadOneCommandThenClose`] in exactly the way that matters — that one is a
    /// close the socket REPORTS, and every path here has always handled it.
    ReadOneCommandThenGoSilent,
}

/// A one-connection node speaking the REAL `Control` handshake, then following `script`.
fn scripted_control_node(script: Script) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let nonce = [23u8; 32];
        match read_frame::<_, Request>(&mut stream) {
            Ok(Request::Hello { .. }) => {}
            other => panic!("expected Hello, got {other:?}"),
        }
        write_frame(
            &mut stream,
            &Response::Welcome {
                proto_version: NODE_PROTO_VERSION,
                nonce,
                features: vec!["observe".to_string()],
            },
        )
        .expect("welcome");
        let Ok(Request::Auth { scope, mac }) = read_frame::<_, Request>(&mut stream) else {
            return;
        };
        assert!(
            auth::verify(KEY, &nonce, NODE_PROTO_VERSION, scope, &mac),
            "the client must present a valid control mac"
        );
        write_frame(&mut stream, &Response::AuthOk { scope }).expect("authok");
        if let Script::ReadOneCommandThenClose | Script::ReadOneCommandThenGoSilent = script {
            match read_frame::<_, Request>(&mut stream) {
                Ok(Request::Command { .. }) => {}
                other => panic!("expected the one Command, got {other:?}"),
            }
        }
        if let Script::ReadOneCommandThenGoSilent = script {
            // …and NOTHING, with the socket still open. No reply, no FIN, no RST — the only
            // failure a `link_is_dead` peek can never see, and the one this node scripts.
            // Parked long past every assertion; the process exits before it does.
            thread::sleep(SILENT_HOLD);
            return;
        }
        // Drop the socket: a clean close, the same FIN a node's own `return` sends.
        drop(stream);
    });
    addr
}

fn a_resting_limit() -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: "never-sent-test-1".to_string(),
        venue: "polymarket".to_string(),
        symbol: "NEVER_SENT_TOKEN".to_string(),
        side: 1,
        qty: 20.0,
        order_type: "limit".to_string(),
        price: Some(0.40),
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

/// **THE DEFECT.** The node closed the link before this command existed, so nothing was
/// written — and saying UNKNOWN ("it may have executed") about a command the node never read
/// is a false statement that costs the caller a round trip and forbids it from simply sending.
///
/// The trigger this was written for was the node's five-minute idle close of an AUTHED control
/// connection; that is fixed at the node (`crate::liveness::AUTHED_IDLE_TIMEOUT`), and a clean
/// close still happens on a restart or a tunnel blip, which is what this scripts.
#[test]
fn a_never_sent_write_is_reported_as_never_sent_not_unknown() {
    let addr = scripted_control_node(Script::CloseAfterAuth);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");
    // Let the FIN land before the command is offered — the whole point is that the close
    // arrived while the worker was parked in its command channel, unread.
    thread::sleep(Duration::from_millis(100));

    let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
    assert_eq!(
        handle.await_outcome(ticket, WAIT),
        Some(CommandOutcome::NeverSent),
        "the link was already closed, so not one byte went — reporting UNKNOWN here is the \
             defect: it forbids the caller from simply sending the command"
    );
    assert!(!handle.is_connected(), "…and the worker is done, so the handle is dead");
}

/// **THE ORDERING that assertion depends on, asserted on its own and many times over.**
///
/// `a_never_sent_write_is_reported_as_never_sent_not_unknown` above ends by reading
/// [`RemoteControlHandle::is_connected`] straight after `await_outcome` answered. That is not
/// two assertions about one fact, it is one assertion about a HAPPENS-BEFORE: the worker must
/// not publish a terminal verdict while it is still advertising a live link.
///
/// It flaked on the shared CI runners — one failure in 10,981 tests, on commits that could not
/// reach this crate — and the loop below is what turns "it flaked" into a measurement. Each
/// round is the same script: connect, let the FIN land, offer one command, wait for the verdict,
/// and read the flag in the same breath.
///
/// ⚠ **What this test is worth is different before and after the fix, and saying so is the
/// point.** Before, it is PROBABILISTIC: the window is the handful of instructions between
/// `record`'s `notify_all` and the `connected.store` after the loop, so a round only fails if
/// the worker is descheduled inside it — which is why the original flaked on a loaded box and
/// never on a quiet one. After, it cannot fail at all: the verdict is published AFTER the store,
/// so there is no window left to lose. A green run here is therefore weak evidence on the old
/// code and a structural guarantee on the new — and the reason the fix is an ORDERING change
/// rather than a poll-until-true in the test above.
#[test]
fn a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected() {
    for round in 0..ORDERING_ROUNDS {
        let addr = scripted_control_node(Script::CloseAfterAuth);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");
        thread::sleep(FIN_SETTLE);

        let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
        // ⚠ SPIN, do not block. `await_outcome(_, WAIT)` parks on the board's condvar, and a
        // parked observer is the WRONG instrument for this race: the worker calls `notify_all`
        // and then keeps its core through `break` and the store, so the sleeper is woken into a
        // world where the flag has already flipped. Pinning to one CPU makes it worse still —
        // the waker never yields. Polling with a ZERO timeout makes the observer already
        // runnable on another core: it needs only the board's mutex, which the worker drops
        // BEFORE it notifies, so it can read the verdict while the worker is still unwinding.
        // Measured: 30 blocking rounds pass on a quiet lane and pinned to one CPU; the spin is
        // what turns the window into something a test can stand on.
        let deadline = Instant::now() + WAIT;
        let outcome = loop {
            if let Some(o) = handle.await_outcome(ticket, Duration::ZERO) {
                break Some(o);
            }
            assert!(Instant::now() < deadline, "round {round}: no verdict within {WAIT:?}");
            std::hint::spin_loop();
        };
        assert_eq!(outcome, Some(CommandOutcome::NeverSent), "round {round}");
        // ⚠ NO retry, NO poll, NO sleep between the two reads. A poll here would make this test
        // pass on the broken ordering and measure nothing — the defect IS that the two answers
        // can disagree for an instant, and an instant is all a caller needs to act on.
        assert!(
            !handle.is_connected(),
            "round {round}: the worker published `{outcome:?}` — a terminal verdict about a \
                 link it had already found dead — while `is_connected()` still said the link was \
                 up. A caller doing the obvious thing (await the outcome, then ask whether to \
                 reconnect) gets those two answers in that order and they contradict each other. \
                 The store must precede the publish, not follow it."
        );
    }
}

/// A command still QUEUED when the worker stops is never-sent too — same evidence (the worker
/// never took it out of the queue), reached through the board's terminal answer rather than a
/// per-command record. Its unique value over the board unit tests above is the COMPOSITION:
/// that the dead-link `break` really reaches `finish(NeverSent)`, and that the command queued
/// behind it is never written.
///
/// ⚠ **Both commands are enqueued while the worker is parked at a START GATE, and that is what
/// makes this test deterministic.** It failed on CI at its SECOND `try_command` with
/// `ControlRejected::Gone`, and the window is not the FIN: `try_send` wakes the worker from
/// inside the call, and the worker's entire life after finding the link dead is `break` →
/// `connected.store(false)` → `record` → `finish` → drop the receiver. So the gap between the
/// two sends is a gap in which the channel can close, and nothing ordered them. The gate
/// ([`RemoteControlHandle::connect_gated`]) takes the scheduler out of the enqueue: neither
/// send can meet a closed channel, because the only thread that can close it has not run.
///
/// ⚠ Two non-fixes, recorded because both are tempting. The sleep is NOT the bug and moving it
/// in either direction changes nothing — it buys margin for the peer's FIN, and the worker is
/// parked (in `recv()`, now at the gate) for the whole of it. And ACCEPTING `Err(Gone)` as a
/// pass would make the test vacuous on precisely the runs where it is informative.
#[test]
fn a_command_queued_behind_a_dead_link_is_never_sent() {
    let addr = scripted_control_node(Script::CloseAfterAuth);
    let start = Arc::new(Barrier::new(2));
    let handle = RemoteControlHandle::connect_gated(
        addr,
        KEY,
        CONTROL_REPLY_TIMEOUT,
        Some(Arc::clone(&start)),
    )
    .expect("the scripted node auths");
    thread::sleep(FIN_SETTLE);

    // ⚠ ENQUEUE, RELEASE, then assert — nothing may panic while the gate is held. A panic here
    // drops the handle, whose `Drop` joins a worker parked at a barrier no one will ever reach:
    // a HUNG test instead of a red one.
    let first = handle.try_command(a_resting_limit());
    let second = handle.try_command(a_resting_limit());
    start.wait();

    let first = first.expect("the queue is open: the worker is parked at the start gate");
    let second = second.expect("the queue is open: the worker is parked at the start gate");
    assert_eq!(handle.await_outcome(first, WAIT), Some(CommandOutcome::NeverSent));
    assert_eq!(
        handle.await_outcome(second, WAIT),
        Some(CommandOutcome::NeverSent),
        "the second never reached the socket at all"
    );
}

/// The gate above, PROVEN rather than assumed: while it is held the worker has taken nothing
/// out of the queue, so the ticket has no verdict at all; released, the ordinary never-sent
/// path runs to its normal answer. Delete the `gate.wait()` from the worker and this fails
/// deterministically — an ungated worker resolves that command in microseconds, so the held
/// read finds a verdict where there must be none.
#[test]
fn the_start_gate_holds_the_worker_until_it_is_released() {
    let addr = scripted_control_node(Script::CloseAfterAuth);
    let start = Arc::new(Barrier::new(2));
    let handle = RemoteControlHandle::connect_gated(
        addr,
        KEY,
        CONTROL_REPLY_TIMEOUT,
        Some(Arc::clone(&start)),
    )
    .expect("the scripted node auths");
    thread::sleep(FIN_SETTLE);

    let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
    // Capture, release, THEN assert — see the sibling test for why.
    let while_held = handle.await_outcome(ticket, GATE_HELD_PROBE);
    start.wait();

    assert_eq!(
        while_held, None,
        "the worker is parked BEFORE the command loop, so it cannot have resolved anything — \
             a verdict here means the gate is not gating and the sibling test is racing again"
    );
    assert_eq!(
        handle.await_outcome(ticket, WAIT),
        Some(CommandOutcome::NeverSent),
        "…and once released it runs the ordinary dead-link path, so the gate changes WHEN the \
             worker starts and nothing else"
    );
}

/// **THE OTHER HALF, and the one that must NOT move.** The node READ this command and the
/// reply was lost: it may have executed. That is `Disconnected`, and every caller's rule for
/// it (never resend; go read the book) is unchanged.
#[test]
fn a_post_send_loss_is_unknown() {
    let addr = scripted_control_node(Script::ReadOneCommandThenClose);
    let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");

    let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
    assert_eq!(
        handle.await_outcome(ticket, WAIT),
        Some(CommandOutcome::Disconnected),
        "the command was WRITTEN — its outcome is genuinely unknown and it must never be \
             reported as never-sent, which would license a resend"
    );
}

/// **THE WRITE HALF'S SILENT DEATH.** The observe half got a deadline and this one did not, so
/// a control link that died WITHOUT a FIN parked the worker in its reply read for the life of
/// the process. Everything downstream then lied by omission: `is_connected` stayed `true` (it
/// is stored `false` only after the loop), so nothing let the handle go and nothing
/// reconnected; every later command sat in the queue behind the wedge; and `vike-cli mcp`
/// answered each of them `"sent": true, "outcome": "unknown", …"it may still execute"` for a
/// command that had provably never left the process — the exact statement this branch exists
/// to eliminate, made permanent.
///
/// ⚠ Note WHICH command gets which verdict, because the pair is the property: the one that was
/// WRITTEN is [`CommandOutcome::Disconnected`] (unknown — it may have executed, so nobody may
/// resend it), and the one still QUEUED behind it is [`CommandOutcome::NeverSent`] (provably
/// not one byte, so `vike-cli mcp`'s `Server::execute` may reconnect and send it). A test that
/// asserted one outcome for both would pass over the bug that matters.
///
/// ⚠ KILL PROOF: delete the `set_read_timeout` in
/// [`RemoteControlHandle::connect_with_reply_timeout`] and this must fail on the FIRST
/// assertion within `WAIT` — the worker's read never returns, so no ticket ever resolves. Run
/// both ways; the commit message carries the outcomes.
#[test]
fn a_silently_dead_control_link_ends_the_worker_instead_of_wedging_it() {
    let addr = scripted_control_node(Script::ReadOneCommandThenGoSilent);
    let handle = RemoteControlHandle::connect_with_reply_timeout(addr, KEY, TEST_REPLY_TIMEOUT)
        .expect("the scripted node auths");

    let written = handle.try_command(a_resting_limit()).expect("the queue is open");
    let queued = handle.try_command(a_resting_limit()).expect("the queue is open");

    assert_eq!(
        handle.await_outcome(written, WAIT),
        Some(CommandOutcome::Disconnected),
        "the node read this one and answered nothing — silence is not evidence that it did \
             not execute, so UNKNOWN is the only honest verdict, and before the deadline existed \
             this ticket resolved to nothing at all"
    );
    assert_eq!(
        handle.await_outcome(queued, WAIT),
        Some(CommandOutcome::NeverSent),
        "…and the one still in the queue behind it provably never reached the socket, which \
             is what lets the caller reconnect and send THAT one"
    );
    assert!(
        !handle.is_connected(),
        "the worker is done, so the handle reports dead and the next offer is Gone — the \
             wedge is self-healing now instead of permanent"
    );
}
