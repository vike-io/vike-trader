//! NEVER-SENT vs UNKNOWN against a scripted node. `vike-cli mcp` re-sends on one and refuses to
//! on the other, so these are the safety argument; the node is scripted because the two cases
//! differ only in WHEN the peer closes (before the command is offered, or after reading it).

use super::*;
use std::net::{SocketAddr, TcpListener};
use std::thread;
use std::time::Instant;

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
/// How long to let a loopback FIN arrive before offering the command. Generous by three orders of
/// magnitude: the point is a link the worker finds ALREADY dead, not one dying under it.
const FIN_SETTLE: Duration = Duration::from_millis(100);
/// How long [`the_start_gate_holds_the_worker_until_it_is_released`] looks for a verdict that
/// must not exist: an UNGATED worker would certainly have produced one by then.
const GATE_HELD_PROBE: Duration = Duration::from_millis(200);
/// Rounds of [`a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected`],
/// sized by cost (a few seconds). Evidence only on the BROKEN ordering; on the fixed one a single
/// round proves it, and the rest keep the ordering from silently coming back.
const ORDERING_ROUNDS: usize = 30;

/// When the scripted node hangs up.
#[derive(Clone, Copy)]
enum Script {
    /// Close CLEANLY straight after `AuthOk`, having read no command (a daemon restart, a tunnel
    /// dropping one link): the client's next command meets a FIN already in its receive queue.
    CloseAfterAuth,
    /// Read ONE command and close WITHOUT answering: whether it executed is unknowable here.
    ReadOneCommandThenClose,
    /// Read ONE command and go SILENT while HOLDING the socket open, no FIN or RST: the silent
    /// death (a laptop sleeping, a VPN re-key under an `ssh -L`), unlike the REPORTED close above.
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
            // …and NOTHING, socket still open: the one failure a `link_is_dead` peek never sees.
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

/// **THE DEFECT.** The node closed the link before this command existed, so nothing was written;
/// UNKNOWN ("it may have executed") would forbid the caller from simply sending it.
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

/// **THE HAPPENS-BEFORE the test above depends on, asserted many times over:** the worker must
/// not publish a terminal verdict while it still advertises a live link. Each round: connect, let
/// the FIN land, offer one command, take the verdict, read `is_connected` in the same breath.
///
/// ⚠ On the old ordering this was PROBABILISTIC (a round failed only if the worker was
/// descheduled between `record`'s `notify_all` and the `connected.store`; it flaked on loaded CI
/// runners). With the store first it cannot fail, which is why the fix is an ORDERING change and
/// not a poll-until-true in the test above.
#[test]
fn a_terminal_verdict_is_never_published_while_the_handle_still_reads_connected() {
    for round in 0..ORDERING_ROUNDS {
        let addr = scripted_control_node(Script::CloseAfterAuth);
        let handle = RemoteControlHandle::connect(addr, KEY).expect("the scripted node auths");
        thread::sleep(FIN_SETTLE);

        let ticket = handle.try_command(a_resting_limit()).expect("the queue is open");
        // ⚠ SPIN, do not block: a parked observer is woken only after the worker has kept its
        // core through the store, so it never sees the window. A ZERO-timeout poll is already
        // runnable on another core and needs only the board's mutex (dropped BEFORE the notify).
        // Measured: 30 blocking rounds pass even on the broken ordering.
        let deadline = Instant::now() + WAIT;
        let outcome = loop {
            if let Some(o) = handle.await_outcome(ticket, Duration::ZERO) {
                break Some(o);
            }
            assert!(Instant::now() < deadline, "round {round}: no verdict within {WAIT:?}");
            std::hint::spin_loop();
        };
        assert_eq!(outcome, Some(CommandOutcome::NeverSent), "round {round}");
        // ⚠ NO retry, poll or sleep between the two reads: the defect IS an instant of
        // disagreement, and a poll would pass on the broken ordering.
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

/// A command still QUEUED when the worker stops is never-sent too, via the board's terminal
/// answer. The value over the board unit tests is the COMPOSITION: the dead-link `break` really
/// reaches `finish(NeverSent)`, and the command queued behind it is never written.
///
/// ⚠ Both commands are enqueued while the worker is parked at a START GATE
/// ([`RemoteControlHandle::connect_gated`]): ungated, the worker could find the link dead and drop
/// its receiver between the two sends, and the second failed on CI with `Gone`. Not fixes: moving
/// the sleep (it only buys margin for the FIN), or accepting `Err(Gone)` (vacuous exactly when
/// informative).
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

    // ⚠ ENQUEUE, RELEASE, then assert: a panic while the gate is held drops the handle, whose
    // `Drop` joins a worker parked at a barrier nobody reaches, a HUNG test instead of a red one.
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

/// The gate, PROVEN: while held the ticket has no verdict at all; released, the ordinary
/// never-sent path runs. Delete the worker's `gate.wait()` and this fails deterministically.
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

/// **THE OTHER HALF, which must NOT move.** The node READ this command and the reply was lost:
/// it may have executed, so `Disconnected` (never resend; go read the book).
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

/// **THE WRITE HALF'S SILENT DEATH.** Without a reply deadline a control link that died WITHOUT
/// a FIN parked the worker forever: `is_connected` stayed `true`, every later command queued
/// behind the wedge, and `vike-cli mcp` answered "outcome unknown" for commands never sent.
///
/// ⚠ The PAIR is the property: the WRITTEN command is [`CommandOutcome::Disconnected`] (may have
/// executed, never resend) and the one QUEUED behind it [`CommandOutcome::NeverSent`] (safe to
/// reconnect and send). One outcome for both would pass over the bug that matters.
///
/// ⚠ KILL PROOF: delete the `set_read_timeout` in [`RemoteControlHandle::connect_gated`] and the
/// FIRST assertion fails within `WAIT` (no ticket ever resolves).
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
