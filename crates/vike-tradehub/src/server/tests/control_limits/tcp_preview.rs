//! The TCP dry-run sizes with what the node PUBLISHED, over a real server and handshake.
use super::*;

/// Complete the real handshake as the WRITE scope over a real socket and return the authed stream.
/// The shape of `server::tests::server_link_liveness`'s `authed_stream`, narrowed to the one scope
/// this test needs.
fn control_stream(addr: SocketAddr, key: &[u8]) -> TcpStream {
    let mut stream = TcpStream::connect(addr).expect("connect");
    write_frame(&mut stream, &Request::Hello { proto_version: NODE_PROTO_VERSION }).expect("hello");
    let nonce = match read_frame::<_, Response>(&mut stream).expect("welcome") {
        Response::Welcome { nonce, .. } => nonce,
        other => panic!("expected Welcome, got {other:?}"),
    };
    let mac = auth::sign(key, &nonce, NODE_PROTO_VERSION, Scope::Write);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Write, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Write } => {}
        other => panic!("expected AuthOk(Write), got {other:?}"),
    }
    stream
}

/// Serve `snap` from a test-owned snapshot cell over the real server (control keyed by
/// `control_key`, a 1000 notional ceiling, an unthrottled rate), complete the WRITE handshake and
/// return the authed stream. No core and no `CommandSink`: a preview lowers nothing.
fn preview_server(
    snap: vike_exec::CoreSnapshot,
    observe_key: &'static [u8],
    control_key: &'static [u8],
) -> TcpStream {
    let cell = Arc::new(arc_swap::ArcSwap::from_pointee(snap));
    let publisher = crate::publish::spawn(cell, None);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral loopback");
    let addr = listener.local_addr().expect("resolve assigned port");
    std::thread::spawn(move || {
        let _ = super::serve(
            listener,
            publisher,
            NodeKeys::new(observe_key.to_vec(), control_key.to_vec()),
            None,
            ControlLimitsConfig { max_notional: Some(1_000.0), rate_per_sec: 1e9 },
            None,
            None,
            None,
        );
    });
    control_stream(addr, control_key)
}

/// Send `cmd` as a `Request::Preview` on `ctl` and return the verdict.
fn preview(ctl: &mut TcpStream, cmd: WireCommand) -> (bool, Option<String>) {
    write_frame(ctl, &Request::Preview(cmd)).expect("preview");
    match read_frame::<_, Response>(ctl).expect("verdict") {
        Response::Preview { accepted, reason } => (accepted, reason),
        other => panic!("expected a Preview verdict, got {other:?}"),
    }
}

/// **The TCP dry-run sizes with the multiplier the node PUBLISHED.** The real server, a real
/// handshake and a test-owned snapshot cell whose one engine block carries a multiplier-100 grid:
/// a `Preview` of an order that is under the ceiling unmultiplied (1 x 20) comes back refused with
/// the multiplier named, and one that is under it either way (1 x 5 x 100 = 500) comes back
/// accepted — so the verdict moved because of the roster and not because the preview refuses
/// everything. No core and no `CommandSink`: a preview lowers nothing.
///
/// ⚠ KILL PROOF: have the `Request::Preview` arm call `limits.preview_vet(&wire_cmd, &[], &orders)`
/// and the first reply flips to accepted.
#[test]
fn a_tcp_preview_sizes_a_submit_with_the_engine_multiplier_the_node_published() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-notional-multiplier-preview";
    let mut snap = vike_exec::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 100.0));
    let mut ctl =
        preview_server(snap, b"observe-key-for-the-notional-multiplier-preview", CONTROL_KEY);

    let (accepted, reason) = preview(&mut ctl, submit(1.0, Some(20.0)));
    assert!(!accepted, "1 x 20 x 100 = 2000 is over the 1000 ceiling");
    let reason = reason.expect("a refusal carries its reason");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size with the published multiplier: {reason}"
    );

    let (accepted, reason) = preview(&mut ctl, submit(1.0, Some(5.0)));
    assert!(accepted, "1 x 5 x 100 = 500 is under the ceiling: {reason:?}");
}

/// **The TCP dry-run sizes a `Modify` with the multiplier of the order the node PUBLISHED.** The
/// real server, a real handshake and a test-owned snapshot cell holding a multiplier-100 engine
/// block, one OPEN order `c1` resting on it and one terminal order `done` — the path
/// `Request::Preview` takes through `PublisherHandle::open_orders_named_by`, which the unit table
/// bypasses. `submit small, modify up`: the modify's 1 x 20 is 20 unmultiplied and 2000 on the
/// engine `c1` rests on, so the preview comes back refused with the multiplier named; a modify that
/// is under the ceiling either way (1 x 5 x 100 = 500) comes back accepted; and a coid the snapshot
/// does not hold, or holds only as a terminal order, is sized at 1.0 as it always was.
///
/// Its body calls no signature this change alters — it drives only the TCP wire and the published
/// cell — so the same test dropped into `main`'s tree compiles and fails for the stated reason: the
/// first verdict is `accepted` there, because that edge sizes every `Modify` at 1.0.
///
/// ⚠ KILL PROOF: have the `Request::Preview` arm hand `preview_vet` an empty `orders` and the first
/// verdict flips to accepted. (The `done` verdict is held by TWO filters, the accessor's and the
/// limiter's own, so it flips only when both `is_terminal` tests go; the accessor's is pinned alone
/// by `open_orders_named_by_answers_the_open_order_a_modify_names_and_nothing_else`.)
#[test]
fn a_tcp_preview_sizes_a_modify_with_the_multiplier_of_the_order_the_node_published() {
    const CONTROL_KEY: &[u8] = b"control-key-for-the-notional-modify-preview";
    let mut snap = vike_exec::CoreSnapshot::empty("hyperliquid", "BTC");
    snap.portfolio.venues.push(block("hyperliquid", "BTC", 100.0));
    snap.orders.push(published_order(
        "c1",
        "hyperliquid",
        None,
        "BTC",
        vike_exec::OrderStatus::Accepted,
    ));
    snap.orders.push(published_order(
        "done",
        "hyperliquid",
        None,
        "BTC",
        vike_exec::OrderStatus::Filled,
    ));
    let mut ctl = preview_server(snap, b"observe-key-for-the-notional-modify-preview", CONTROL_KEY);

    let (accepted, reason) = preview(&mut ctl, modify_to("c1", 1.0, 20.0));
    assert!(!accepted, "1 x 20 x 100 = 2000 is over the 1000 ceiling for the order c1 rests on");
    let reason = reason.expect("a refusal carries its reason");
    assert!(
        reason.contains("2000.00") && reason.contains("contract multiplier 100"),
        "the preview must size the modify with the published order's multiplier: {reason}"
    );

    let (accepted, reason) = preview(&mut ctl, modify_to("c1", 1.0, 5.0));
    assert!(accepted, "1 x 5 x 100 = 500 is under the ceiling: {reason:?}");

    let (accepted, reason) = preview(&mut ctl, modify_to("never-published", 1.0, 20.0));
    assert!(accepted, "an order the snapshot does not hold is sized at 1.0: {reason:?}");

    let (accepted, reason) = preview(&mut ctl, modify_to("done", 1.0, 20.0));
    assert!(accepted, "a TERMINAL order lends no multiplier: {reason:?}");
}
