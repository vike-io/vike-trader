//! End-to-end CONTROL-server tests (headless two-layer plan, Layer 2, PR-12) — loopback only, no
//! creds, no external network, no `polymarket` feature.
//!
//! A real PAPER node ([`vike_mount::build_paper_maker_core`]) + the real [`vike_tradehub::server`] +
//! [`vike_tradehub::publish`] fan-out are bound on an ephemeral `127.0.0.1:0` port, with a CONTROL key
//! in the [`NodeKeys`] and the core's `CommandSink` threaded into `serve`. A `Scope::Write` peer
//! submits/cancels orders over a SEPARATE, NEVER-subscribed request/response connection (a subscribed
//! connection is a one-way push pipe); a separate observe connection verifies the order reaches the
//! pushed snapshot. The paper mount has no feed, so the maker never quotes — the ONLY orders in the
//! book are the operator's, which makes the snapshot assertions deterministic.
//!
//! What is proven:
//! - a `Submit` under `Control` acks (echoing the pre-minted coid) and appears in a pushed snapshot;
//! - a `Command` is refused (`Error`) when the node has a control key but NO `CommandSink`;
//! - a `Control` auth is denied when the node has no control key (control disabled);
//! - an `Observe`-authenticated peer's `Command` is denied (read-only);
//! - a `Submit` with an EMPTY `client_order_id` is rejected (the idempotency policy) and books nothing;
//! - a duplicate coid submits exactly ONE order (the registry is coid-keyed / idempotent);
//! - the high-level [`vike_tradehub_client::RemoteControlHandle`] drives a submit end-to-end;
//! - a `Preview` (v3 dry-run) under `Control` returns the gate verdict WITHOUT executing (no order
//!   is booked), is refused for an `Observe` peer, and drives via
//!   [`vike_tradehub_client::preview_command`];
//! - a `Command` carrying an operator/agent RATIONALE (v4) is accepted and that rationale reaches
//!   the AUDIT record — SANITIZED (see [`audit_capture`] below for how the record is observed);
//! - ⚠ each command's outcome is reported for THAT command: a refused command followed by an
//!   accepted one on the SAME handle resolves to a refusal and an acceptance respectively, while
//!   the separate LATCHED `last_error` status view keeps the refusal until it is explicitly
//!   dismissed (`a_refusal_is_not_reported_again_for_the_next_accepted_command`);
//! - the STRATEGY verbs (split-plane B4): a wire `UpdateParams` re-tunes a REAL mounted
//!   `SpreadMaker` through the one acceptance path and is audited as `"update_params"`; an
//!   undecodable params payload is refused at the lowering edge; `StrategyStatus` on an
//!   identity-less node is an honest error (the mounted-truth answer lives in
//!   `observe_roundtrip.rs`, whose node publishes an identity block).

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use vike_mount::{MakerMount, MakerMountConfig, build_paper_maker_core};
use vike_tradehub::{publish, server};
use vike_tradehub_client::auth;
use vike_tradehub_client::proto::{
    NODE_PROTO_VERSION, Request, Response, Scope, read_frame, write_frame,
};
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};
use vike_tradehub_client::{
    CommandOutcome, NodeKeys, RemoteControlHandle, RemoteCoreHandle, preview_command,
};

use audit_capture::{audit_entry_for, audit_entry_with_kind, test_init};

const TOKEN: &str = "CONTROL_ROUNDTRIP_TOKEN";
/// Far-future resolution so the A-S horizon is positive (mirrors the vike-mount offline mount test).
const RESOLUTION_TS: i64 = 3_000_000_000;
const OBSERVE_KEY: &[u8] = b"observe-secret-key-for-control-tests";
const CONTROL_KEY: &[u8] = b"control-secret-key-for-control-tests";
/// Deadline for `RemoteControlHandle::await_outcome` — generous, because it is a deadline and not a
/// sleep (the wait returns the instant that command's reply lands over loopback).
const OUTCOME_WAIT: Duration = Duration::from_secs(5);

/// Observing the AUDIT trail from an integration test.
///
/// `audit::record` emits ONE `tracing::info!` event, and it is emitted on the SERVER's connection
/// thread — so `tracing::subscriber::with_default` (a THREAD-LOCAL dispatcher) can never see it. The
/// capture has to be the process-global subscriber, and `tracing-subscriber` is not a dependency of
/// this crate, so this is a minimal hand-rolled [`Subscriber`] that is `enabled` ONLY for the
/// `vike_tradehub::audit` target and appends each event's `(kind, coid, reason)` to a shared buffer.
///
/// Every test in this file calls [`test_init`] (in place of `vike_log::test_init`) so the install can
/// never lose the one-global-subscriber race to another test's initializer. The trade is deliberate:
/// this binary's console logging is off, and the audit record — the thing under test — is asserted
/// on directly instead of eyeballed.
mod audit_capture {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Mutex, Once, OnceLock};

    use tracing::field::{Field, Visit};
    use tracing::{Event, Metadata, Subscriber, span};

    /// The tracing target `vike_tradehub::audit`'s events carry (the module path).
    const AUDIT_TARGET: &str = "vike_tradehub::audit";

    /// One captured audit record: the command verb, the client-order-id, and the RECORDED rationale
    /// (`None` when the event carried no `reason` field at all).
    #[derive(Debug, Clone, PartialEq)]
    pub struct AuditEntry {
        pub kind: String,
        pub coid: String,
        pub reason: Option<String>,
    }

    fn captured() -> &'static Mutex<Vec<AuditEntry>> {
        static LOG: OnceLock<Mutex<Vec<AuditEntry>>> = OnceLock::new();
        LOG.get_or_init(|| Mutex::new(Vec::new()))
    }

    /// Install the capture subscriber exactly once for this test binary. Best-effort by design: a
    /// failed install would make the capture EMPTY, which the audit test then fails on loudly rather
    /// than passing vacuously.
    pub fn test_init() {
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            let _ = tracing::subscriber::set_global_default(AuditCapture {
                next_span: AtomicU64::new(1), // span::Id::from_u64 panics on 0
            });
        });
    }

    /// The audit entry recorded for `coid`, waited on briefly. (`audit::record` runs BEFORE the
    /// server writes its `Ack`, so it is already present by the time a caller has read one — the
    /// wait is belt-and-braces, not a race the assertion depends on.)
    pub fn audit_entry_for(coid: &str) -> Option<AuditEntry> {
        for _ in 0..200 {
            if let Some(e) =
                captured().lock().expect("audit capture poisoned").iter().find(|e| e.coid == coid)
            {
                return Some(e.clone());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    /// The audit entry recorded with `kind`, waited on briefly — for the ACCOUNT-WIDE verbs, whose
    /// coid is empty by convention (several tests share the empty coid, so the verb is the
    /// discriminating key for them). Same wait rationale as [`audit_entry_for`].
    pub fn audit_entry_with_kind(kind: &str) -> Option<AuditEntry> {
        for _ in 0..200 {
            if let Some(e) =
                captured().lock().expect("audit capture poisoned").iter().find(|e| e.kind == kind)
            {
                return Some(e.clone());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        None
    }

    struct AuditCapture {
        next_span: AtomicU64,
    }

    impl Subscriber for AuditCapture {
        /// ONLY the audit target — everything else in the process is dropped at the callsite, so the
        /// buffer stays tiny and the capture cannot be swamped by core/publisher chatter.
        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            metadata.target() == AUDIT_TARGET
        }
        fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(self.next_span.fetch_add(1, Ordering::Relaxed))
        }
        fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
        fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut visitor = FieldVisitor::default();
            event.record(&mut visitor);
            captured().lock().expect("audit capture poisoned").push(AuditEntry {
                kind: visitor.kind,
                coid: visitor.coid,
                reason: visitor.reason,
            });
        }
        fn enter(&self, _: &span::Id) {}
        fn exit(&self, _: &span::Id) {}
    }

    /// Pull the three string fields off the audit event. `reason` is recorded as `Option<&str>`,
    /// which tracing records as the inner `&str` when `Some` and emits NO field at all when `None` —
    /// so an absent `reason` here means the record genuinely carried none.
    #[derive(Default)]
    struct FieldVisitor {
        kind: String,
        coid: String,
        reason: Option<String>,
    }

    impl Visit for FieldVisitor {
        fn record_str(&mut self, field: &Field, value: &str) {
            match field.name() {
                "kind" => self.kind = value.to_string(),
                "coid" => self.coid = value.to_string(),
                "reason" => self.reason = Some(value.to_string()),
                _ => {}
            }
        }
        // The `message` / `?peer` fields arrive here; nothing to capture from them.
        fn record_debug(&mut self, _: &Field, _: &dyn std::fmt::Debug) {}
    }
}

/// A v4 [`Request::Command`] with NO rationale — the shape every pre-v4 caller sent, and what all
/// but the audit test below use.
fn command(cmd: WireCommand) -> Request {
    Request::Command { cmd, reason: None }
}

/// Poll `cond` up to `secs`, returning whether it became true — the core folds on its own thread, the
/// publisher fans out on another, and the observe client receives on a third, so this waits for the
/// push to propagate.
fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Build a PAPER node and start the server over it ([`serve_core`], with `keys` and `with_sink`).
/// Returns the live mount (its `CoreHandle` keeps the core running) and the assigned address.
fn spawn_node(keys: NodeKeys, with_sink: bool) -> (MakerMount, SocketAddr) {
    let cfg = MakerMountConfig::outcome_token("polymarket", TOKEN, Some(RESOLUTION_TS));
    let mount = build_paper_maker_core(&cfg);
    let addr = serve_core(&mount.handle, keys, with_sink);
    (mount, addr)
}

/// Start the REAL server — the same `serve()` the daemon runs — over `handle`'s published snapshot
/// on an ephemeral loopback port with the given `keys`, and return the assigned address. When
/// `with_sink`, the core's `CommandSink` is threaded into `serve` (control ENABLED); otherwise `None`
/// (control refused per-command).
fn serve_core(handle: &vike_core::CoreHandle, keys: NodeKeys, with_sink: bool) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    let publisher = publish::spawn(handle.snapshot_cell(), None);
    let commands = if with_sink { Some(handle.command_sink()) } else { None };
    thread::spawn(move || {
        // Default edge limits (no size cap, default 20/s rate) — the caller-owned config seam
        // (audit F13), matching what the daemon resolves with both env knobs unset.
        // No settings source: these suites are about the CONTROL path; the SettingsShow arm
        // answers its honest no-source error (pinned in tests/daemon/settings_show.rs).
        let _ = server::serve(
            listener,
            publisher,
            keys,
            commands,
            server::control::ControlLimitsConfig::default(),
            None,
            // No `AccountAdminSource`: the account capability is an ABSENCE on every box that
            // has not DECLARED a barrier, which is every fixture here and every shipped box today.
            None,
            None,
        );
    });
    addr
}

/// A resting limit far from any market — it stays WORKING (never fills without a feed), so the
/// snapshot deterministically carries exactly the orders we submit.
fn resting_submit(coid: &str) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.to_string(),
        venue: "polymarket".to_string(),
        symbol: TOKEN.to_string(),
        side: 1,
        qty: 20.0,
        order_type: "limit".to_string(),
        price: Some(0.40),
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

/// Send [`resting_submit`]`(coid)` as a `Command` on the authed control stream `ctl` and require
/// the node's `Ack` to echo the pre-minted coid.
fn submit_acked(ctl: &mut TcpStream, coid: &str) {
    write_frame(ctl, &command(resting_submit(coid))).expect("command");
    match read_frame::<_, Response>(ctl).expect("ack") {
        Response::Ack { coid: acked } => assert_eq!(acked, coid, "the pre-minted coid is echoed"),
        other => panic!("expected Ack{{{coid}}}, got {other:?}"),
    }
}

/// Open a SEPARATE observe connection to `addr` and wait (up to 5 s) until its pushed snapshot
/// holds the order `coid`, failing with `never` otherwise; the observer is returned for the
/// caller's own asserts.
fn observe_order(addr: SocketAddr, coid: &str, never: &str) -> RemoteCoreHandle {
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(5, || observer.snapshot().orders.iter().any(|o| o.client_order_id == coid)),
        "{never}"
    );
    observer
}

/// `Hello` -> read the `Welcome` challenge nonce.
fn hello_welcome(stream: &mut TcpStream) -> [u8; 32] {
    write_frame(stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    match read_frame::<_, Response>(stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    }
}

/// Complete the handshake as `Control` and return the authed stream (NOT subscribed — control stays
/// in the request/response loop).
fn control_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let nonce = hello_welcome(&mut stream);
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Write } => {}
        other => panic!("expected AuthOk(Control), got {other:?}"),
    }
    stream
}

/// Complete the handshake as `Observe` and return the authed stream (NOT subscribed).
fn observe_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    let nonce = hello_welcome(&mut stream);
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Read } => {}
        other => panic!("expected AuthOk(Observe), got {other:?}"),
    }
    stream
}

#[test]
fn control_submit_acks_and_appears_in_snapshot() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut ctl = control_stream(addr, CONTROL_KEY);
    submit_acked(&mut ctl, "c-1");

    // A SEPARATE observe connection: the control-submitted order reaches the pushed snapshot.
    let observer =
        observe_order(addr, "c-1", "the control-submitted order never reached the pushed snapshot");
    let snap = observer.snapshot();
    let ov = snap.orders.iter().find(|o| o.client_order_id == "c-1").expect("order present");
    assert_eq!(ov.symbol, TOKEN, "routed to the mount symbol");
    assert_eq!(ov.side, 1, "side survived the lower + wire round-trip");
}

#[test]
fn control_denied_when_sink_is_none() {
    test_init();
    // Control KEY present (so the Control handshake succeeds) but NO CommandSink threaded in.
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), false);

    let mut ctl = control_stream(addr, CONTROL_KEY);
    write_frame(&mut ctl, &command(resting_submit("c-none"))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("response") {
        // The sink-gate short-circuits BEFORE lowering, so nothing reaches the core (no order).
        Response::Error(msg) => assert!(msg.contains("not enabled"), "reason: {msg}"),
        other => panic!("a command with no sink must be Error, got {other:?}"),
    }
}

#[test]
fn control_auth_denied_when_control_key_absent() {
    test_init();
    // Control KEY absent — even with a sink present, the Control handshake is refused FIRST.
    let (_mount, addr) = spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), Vec::new()), true);

    let mut stream = TcpStream::connect(addr).expect("connect");
    let nonce = hello_welcome(&mut stream);
    let mac = auth::sign(CONTROL_KEY, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("control disabled"), "reason: {reason}")
        }
        other => panic!("Control auth with no control key must be AuthDenied, got {other:?}"),
    }
}

#[test]
fn observe_scope_command_is_denied() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    // Authenticated as Observe, a Command is refused (read-only) even though control IS enabled.
    let mut stream = observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut stream, &command(WireCommand::Cancel("x".into()))).expect("command");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("read-only"), "reason: {reason}")
        }
        other => panic!("a Command under Observe must be AuthDenied, got {other:?}"),
    }
}

#[test]
fn submit_with_empty_coid_is_rejected() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut ctl = control_stream(addr, CONTROL_KEY);
    // Empty coid → rejected by the server BEFORE it reaches the core (the idempotency policy).
    write_frame(&mut ctl, &command(resting_submit(""))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("response") {
        Response::Error(msg) => assert!(msg.contains("client_order_id"), "reason: {msg}"),
        other => panic!("an empty-coid submit must be Error, got {other:?}"),
    }

    // A valid sentinel proves the pipeline still works AND lets us assert exactly-one order (the
    // paper maker never quotes without a feed, so the ONLY orders are the ones we submit).
    submit_acked(&mut ctl, "sentinel");
    let observer = observe_order(addr, "sentinel", "the sentinel never reached the snapshot");
    assert_eq!(
        observer.snapshot().orders.len(),
        1,
        "the empty-coid submit created NO order — only the sentinel exists"
    );
}

#[test]
fn duplicate_coid_is_idempotent() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let mut ctl = control_stream(addr, CONTROL_KEY);
    // Send the SAME coid twice — the server lowers + acks each, but the core registry is coid-keyed,
    // so the snapshot shows exactly one order.
    for _ in 0..2 {
        submit_acked(&mut ctl, "dup-1");
    }

    let observer =
        observe_order(addr, "dup-1", "the duplicate-coid order never reached the snapshot");
    // Settle the second submit's fold, then assert exactly one order for that coid (and no others).
    thread::sleep(Duration::from_millis(200));
    let snap = observer.snapshot();
    assert_eq!(
        snap.orders.iter().filter(|o| o.client_order_id == "dup-1").count(),
        1,
        "a duplicate coid is idempotent — exactly one registered order"
    );
    assert_eq!(snap.orders.len(), 1, "no other order exists");
}

#[test]
fn remote_control_handle_submits_and_is_observed() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    // The high-level client: connects under Control (no subscribe), sends over its worker thread.
    let control = RemoteControlHandle::connect(addr, CONTROL_KEY).expect("control connect + auth");
    assert!(control.is_connected(), "the worker thread is live right after connect");
    control.try_command(resting_submit("rc-1")).expect("enqueue submit");

    let _observer = observe_order(
        addr,
        "rc-1",
        "the RemoteControlHandle-submitted order never reached the pushed snapshot",
    );
    assert!(control.is_connected(), "still connected after a successful command");
    assert!(control.last_error().is_none(), "a successful submit records no error");
    // Dropping `control` here joins its worker — exercising the Drop wake path (idle `recv` + socket).
}

/// ⚠ **The stale-refusal defect.** A command's outcome must be reported for THAT command and no
/// other.
///
/// The handle's `last_error` LATCHES: it is written when the node replies `Response::Error` and
/// never cleared. While that was the only outcome surface, `vike-cli trade`'s `run_write` and
/// `vike-cli mcp`'s `Server::execute` polled it after every write — so once ONE command was
/// refused, every later command in the session reported the same refusal, INCLUDING commands the
/// node accepted and executed.
///
/// It points the dangerous way. An operator fat-fingers an oversized order, gets a correct
/// refusal, then types `market-exit` — the panic button. The REPL says it was rejected. It ran.
/// They now believe they still hold a position they have closed, or they close it twice.
///
/// The fix gives each command an identity: a send returns a `CommandTicket`, and `await_outcome`
/// resolves THAT ticket. The latch stays exactly as it was for the `vike-app` status strip, which
/// wants a persistent "most recent error" and now has `clear_last_error` as its explicit dismiss —
/// both halves are asserted below, because the point is that they are DIFFERENT questions.
///
/// RED before the fix (`cabb2f17`, the CI box): `left: Some("remote submit requires a pre-minted
/// client_order_id"), right: None` — the accepted command reported the refused one's error.
#[test]
fn a_refusal_is_not_reported_again_for_the_next_accepted_command() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);
    let control = RemoteControlHandle::connect(addr, CONTROL_KEY).expect("control connect + auth");

    // 1. A command the node REFUSES — a submit with no pre-minted coid (the idempotency policy).
    //    It is refused at the server's edge, so it books nothing.
    let refused = control.try_command(resting_submit("")).expect("enqueue the refused submit");
    match control.await_outcome(refused, OUTCOME_WAIT).expect("the node answered the first command")
    {
        CommandOutcome::Refused(msg) => {
            assert!(msg.contains("pre-minted client_order_id"), "the node's own reason: {msg}");
        }
        other => panic!("a submit with no coid must be refused, got {other:?}"),
    }

    // 2. A command the node ACCEPTS, on the SAME handle, immediately after.
    let accepted = control.try_command(resting_submit("stale-2")).expect("enqueue the good submit");
    let reported_for_the_second = control
        .await_outcome(accepted, OUTCOME_WAIT)
        .expect("the node answered the second command");
    assert_eq!(
        reported_for_the_second,
        CommandOutcome::Accepted { coid: "stale-2".to_string() },
        "the SECOND command was ACCEPTED and EXECUTED by the node — reporting the FIRST command's \
         refusal for it is the stale-refusal defect"
    );

    // …and it really did execute: the order reaches the core and a pushed snapshot, while the
    // refused one booked nothing. Without this the assertion above could pass on a node that
    // acked and dropped.
    let observer =
        observe_order(addr, "stale-2", "the accepted submit must actually reach the core");
    assert_eq!(observer.snapshot().orders.len(), 1, "the refused submit booked nothing");

    // 3. The GUI's LATCHED view is deliberately UNCHANGED — it still carries the refusal after the
    //    accepted command, because a status strip must not lose an error between repaints. That is
    //    exactly why it could never be the per-command answer, and why the fix added a ticket
    //    rather than making this accessor consuming.
    assert_eq!(
        control.last_error().as_deref(),
        Some("remote submit requires a pre-minted client_order_id"),
        "the status-strip latch keeps the most recent refusal"
    );
    control.clear_last_error();
    assert_eq!(
        control.last_error(),
        None,
        "…and the explicit dismiss is the only thing clearing it"
    );
}

#[test]
fn preview_returns_verdict_without_executing() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    // On ONE control connection (FIFO into the core), Preview a command, then submit a real
    // sentinel. Since Preview never touches the core, only the sentinel is ever booked — proving the
    // dry-run executed nothing.
    let mut ctl = control_stream(addr, CONTROL_KEY);
    write_frame(&mut ctl, &Request::Preview(resting_submit("pv-1"))).expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("preview reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "a valid, within-limits submit previews as accepted");
            assert_eq!(reason, None, "an accepted verdict carries no reason");
        }
        other => panic!("expected Preview, got {other:?}"),
    }
    submit_acked(&mut ctl, "sentinel");

    let observer = observe_order(addr, "sentinel", "the sentinel never reached the snapshot");
    let snap = observer.snapshot();
    assert!(
        !snap.orders.iter().any(|o| o.client_order_id == "pv-1"),
        "the previewed command must NOT have been booked"
    );
    assert_eq!(snap.orders.len(), 1, "only the sentinel exists — the preview executed nothing");
}

#[test]
fn preview_under_observe_is_denied() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    // An Observe peer may not Preview a command (same scope gate as `Command`).
    let mut stream = observe_stream(addr, OBSERVE_KEY);
    write_frame(&mut stream, &Request::Preview(WireCommand::Cancel("x".into()))).expect("preview");
    match read_frame::<_, Response>(&mut stream).expect("response") {
        Response::AuthDenied { reason } => {
            assert!(reason.contains("read-only"), "reason: {reason}")
        }
        other => panic!("a Preview under Observe must be AuthDenied, got {other:?}"),
    }
}

#[test]
fn preview_command_helper_returns_accepted_and_books_nothing() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    // The high-level standalone helper: a fresh short-lived Control connection, one Preview, verdict.
    let (accepted, reason) =
        preview_command(addr, CONTROL_KEY, &resting_submit("helper-pv")).expect("preview_command");
    assert!(accepted, "a valid submit previews as accepted");
    assert_eq!(reason, None);

    // Nothing was executed: a fresh observer sees an empty order book (the paper maker never quotes).
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    // Give any (erroneous) fold a chance to land, then assert the book stayed empty.
    thread::sleep(Duration::from_millis(200));
    assert!(observer.snapshot().orders.is_empty(), "preview_command must not book any order");

    // A wrong control key makes the helper's handshake fail with PermissionDenied.
    let err = preview_command(addr, b"wrong-key", &resting_submit("nope"))
        .expect_err("wrong key must deny");
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
}

/// The v4 acceptance test: a rationale supplied with a command REACHES the audit record, SANITIZED —
/// and one supplied with nothing records no rationale at all. The dirty input is the log-injection
/// shape the sanitizer exists to defeat (a newline plus an audit-shaped forged JSON object, a tab and
/// a NUL): what lands in the trail must be one flat line with the payload text intact but every
/// control character gone.
#[test]
fn a_command_rationale_reaches_the_audit_record_sanitized() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);
    let mut ctl = control_stream(addr, CONTROL_KEY);

    let dirty = "flat before CPI\n{\"kind\":\"forged\",\"coid\":\"evil\"}\ttail\u{0}end";
    write_frame(
        &mut ctl,
        &Request::Command { cmd: resting_submit("why-1"), reason: Some(dirty.to_string()) },
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "why-1", "a rationale does not change the coid"),
        other => panic!("expected Ack{{why-1}}, got {other:?}"),
    }

    let entry = audit_entry_for("why-1").expect("the accepted command produced an audit record");
    assert_eq!(entry.kind, "submit", "the verb is still recorded");
    let recorded = entry.reason.expect("the audit record carries the rationale");
    assert_eq!(
        recorded, "flat before CPI{\"kind\":\"forged\",\"coid\":\"evil\"}tailend",
        "the payload text survives; every control character is stripped"
    );
    assert!(
        !recorded.contains('\n') && !recorded.contains('\r') && !recorded.contains('\t'),
        "no control character reaches the structured audit line: {recorded:?}"
    );

    // …and a command with NO rationale records none (byte-identical to the pre-v4 audit line).
    submit_acked(&mut ctl, "why-none");
    let entry = audit_entry_for("why-none").expect("audit record for the reason-less command");
    assert_eq!(entry.reason, None, "an absent rationale logs no `reason` field at all");

    // A whitespace-only rationale is NO rationale — never an empty `reason` field.
    write_frame(
        &mut ctl,
        &Request::Command { cmd: resting_submit("why-blank"), reason: Some("  \n\t ".to_string()) },
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "why-blank"),
        other => panic!("expected Ack{{why-blank}}, got {other:?}"),
    }
    assert_eq!(
        audit_entry_for("why-blank").expect("audit record").reason,
        None,
        "a blank rationale sanitizes to None, not Some(\"\")"
    );

    // The rationale is AUDIT-ONLY: all three orders booked normally and nothing about them changed.
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(5, || observer.snapshot().orders.len() == 3),
        "all three rationale-carrying submits must book exactly like a bare one"
    );
}

/// The client-side twin: [`RemoteControlHandle::try_command_with_reason`] carries the rationale over
/// its worker thread into the same audit record, while the unchanged [`RemoteControlHandle::try_command`]
/// signature keeps sending `reason: None` (the GUI observer path, which needed no change at v4).
#[test]
fn remote_control_handle_carries_a_reason_and_the_bare_call_carries_none() {
    test_init();
    let (_mount, addr) =
        spawn_node(NodeKeys::new(OBSERVE_KEY.to_vec(), CONTROL_KEY.to_vec()), true);

    let control = RemoteControlHandle::connect(addr, CONTROL_KEY).expect("control connect + auth");
    control
        .try_command_with_reason(resting_submit("rc-why"), Some("agent: mean-revert entry".into()))
        .expect("enqueue submit with a reason");
    control.try_command(resting_submit("rc-bare")).expect("enqueue submit with no reason");

    assert_eq!(
        audit_entry_for("rc-why").expect("audit record").reason.as_deref(),
        Some("agent: mean-revert entry")
    );
    assert_eq!(audit_entry_for("rc-bare").expect("audit record").reason, None);
    assert!(control.last_error().is_none(), "both submits were accepted");
}

#[cfg(test)]
#[path = "control_roundtrip/strategy_verbs.rs"]
mod strategy_verbs;
