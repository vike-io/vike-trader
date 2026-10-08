//! The TP/SL BRACKET: default account only, over a real core.

use super::*;

// ---------------------------------------------------------------------------------------------
// The TP/SL BRACKET — default account only, over a real core
// ---------------------------------------------------------------------------------------------
//
// ⚠ The TWO-ACCOUNT refusal has no test in this file, and cannot cheaply have one: the paper
// fixture builds ONE engine per distinct venue (`vike_mount`'s `build_paper_multi_strategy_core_with`
// does not split a venue by account), so this node never publishes two route keys of one venue.
// That verdict is `server::refusal::account_refusal`'s, pinned by its unit suite
// (`a_bracket_goes_only_to_a_venue_whose_one_account_is_the_default` and its siblings), and the
// gate being ON THE PATH is what the submit/mount/reduce account tests above prove over the wire.
// The refusals that ARE reachable here — the empty roster, the values, a symbol the engine does
// not trade, and an engine mounted on binance's spot lane — are proven below as a client sees
// them: `Response::Error`, never an `Ack`, and the same sentence on a Preview.

/// A bracket on `venue`/`symbol`. The limit entry at 0.10 sits far under the 0.50 bar the harness
/// drives, so it never fills, and both exits stay HELD.
fn bracket_on(venue: &str, symbol: &str) -> WireCommand {
    WireCommand::Bracket(WireBracketSpec {
        venue: venue.to_string(),
        symbol: symbol.to_string(),
        side: 1,
        qty: 20.0,
        entry_price: Some(0.10),
        stop_loss: 0.05,
        take_profit: 0.90,
    })
}

/// **A bracket books its entry on the engine it names and holds both exits off the venue.** The
/// `Ack` echoes no coid (the runtime mints all three). The operator's handle is the ENTRY's id off
/// the snapshot, and cancelling it drops both held exits.
///
/// The second venue is the load-bearing one: the primary is polymarket, so a bracket that failed to
/// route would land there.
#[test]
fn a_bracket_books_its_entry_and_holds_both_exits_on_the_engine_it_names() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-held");
    let (mut ctl, features) = control_stream(addr);
    assert!(features.iter().any(|f| f == FEATURE_BRACKET), "advertised: {features:?}");

    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "", "the runtime mints the ids after the Ack"),
        other => panic!("expected Ack, got {other:?}"),
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let held = |s: &vike_tradehub_client::wire::WireSnapshot| {
        s.held_exits.iter().filter(|h| h.symbol == SECOND_SYMBOL).count()
    };
    assert!(
        wait_until(10, || held(&observer.snapshot()) == 2),
        "both exits must be HELD off the venue until the entry fills"
    );
    let snap = observer.snapshot();
    let parent = snap
        .held_exits
        .iter()
        .find(|h| h.symbol == SECOND_SYMBOL)
        .and_then(|h| h.parent_order_id.clone())
        .expect("a held exit names its entry");
    let entry = snap
        .orders
        .iter()
        .find(|o| o.client_order_id == parent)
        .expect("the entry is in the order registry");
    assert_eq!(entry.venue, SECOND_VENUE, "owned by the engine the bracket named, not the primary");
    assert_eq!((entry.side, entry.order_type.as_str()), (1, "limit"));
    assert!(
        snap.held_exits
            .iter()
            .filter(|h| h.symbol == SECOND_SYMBOL)
            .all(|h| h.side == -1 && h.parent_order_id.as_deref() == Some(parent.as_str())),
        "both exits close the entry and name it"
    );

    write_frame(&mut ctl, &command(WireCommand::Cancel(parent.clone()))).expect("cancel");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("expected Ack for the entry's cancel, got {other:?}"),
    }
    assert!(
        wait_until(10, || held(&observer.snapshot()) == 0),
        "cancelling the entry drops the exits it held"
    );
    mount.handle.shutdown_and_join();
}

/// **A bracket the venue cannot hold is refused WHOLE, and no exit is stranded.** Polymarket's
/// capability row has no `stop` kind, so the core's preflight refuses all three legs atomically.
/// The node has already sent `Ack` (this is the core's verdict, not the edge's), and the snapshot
/// shows three rejected orders and nothing held.
#[test]
fn a_bracket_its_venue_cannot_hold_is_refused_whole_and_strands_no_exit() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-refused");
    let (mut ctl, _features) = control_stream(addr);
    write_frame(&mut ctl, &command(bracket_on(PRIMARY_VENUE, PRIMARY_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { .. } => {}
        other => panic!("expected Ack, got {other:?}"),
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let rejected = |s: &vike_tradehub_client::wire::WireSnapshot| {
        s.orders
            .iter()
            .filter(|o| o.symbol == PRIMARY_SYMBOL && o.qty > 10.0 && o.status == "Rejected")
            .count()
    };
    assert!(wait_until(10, || rejected(&observer.snapshot()) == 3), "entry, SL and TP all refused");
    let snap = observer.snapshot();
    assert!(!snap.held_exits.iter().any(|h| h.symbol == PRIMARY_SYMBOL), "no exit is left held");
    assert!(
        snap.orders
            .iter()
            .filter(|o| o.symbol == PRIMARY_SYMBOL && o.qty > 10.0)
            .all(|o| o.status == "Rejected"),
        "no leg is working"
    );
    mount.handle.shutdown_and_join();
}

/// The well-formed second-venue bracket with one field changed by `f`.
fn second_venue_bracket_with(f: fn(&mut WireBracketSpec)) -> WireCommand {
    let WireCommand::Bracket(mut b) = bracket_on(SECOND_VENUE, SECOND_SYMBOL) else {
        unreachable!("bracket_on builds a bracket")
    };
    f(&mut b);
    WireCommand::Bracket(b)
}

/// The three brackets the ordinary fixture refuses for their VALUES or their ENGINE, each beside
/// the needle its refusal must carry: a symbol the binance engine does not trade (it is mounted on
/// [`SECOND_SYMBOL`], so the refusal names that symbol — a bracket goes to that one engine, which
/// would place it on its own instrument), a `side` that is neither +1 nor -1, and an INVERTED long
/// (a stop above its entry).
fn unhonourable_brackets() -> Vec<(&'static str, WireCommand, &'static str)> {
    vec![
        (
            "a symbol its engine does not trade",
            bracket_on(SECOND_VENUE, SECOND_SPOT_SYMBOL),
            "trades `VR_ROUTE_BTC.P`, not `VR_ROUTE_BTC`",
        ),
        ("side 0", second_venue_bracket_with(|b| b.side = 0), "`side`"),
        ("an inverted long", second_venue_bracket_with(|b| b.stop_loss = 0.20), "inverted"),
    ]
}

/// ⚠ **A bracket the node cannot honour is REFUSED BEFORE THE ACK, and books nothing.** Each of
/// [`unhonourable_brackets`] is a refusal a client meets as `Response::Error` on a live connection,
/// where it would otherwise have been Acked and failed at RELEASE, with the position already open.
///
/// The well-formed bracket sent last on the same connection proves both halves of "books nothing":
/// the connection survives each refusal, and the snapshot then holds exactly ONE bracket's exits.
#[test]
fn a_bracket_the_node_cannot_honour_is_refused_before_the_ack_and_books_nothing() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-unhonourable");
    let (mut ctl, _features) = control_stream(addr);

    for (label, cmd, needle) in unhonourable_brackets() {
        write_frame(&mut ctl, &command(cmd)).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Error(msg) => assert!(msg.contains(needle), "{label}: {msg}"),
            other => panic!("{label} must be REFUSED BEFORE THE ACK, got {other:?}"),
        }
    }

    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, ""),
        other => panic!("the well-formed bracket must be Acked, got {other:?}"),
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    let held = |s: &vike_tradehub_client::wire::WireSnapshot| {
        s.held_exits.iter().filter(|h| h.symbol == SECOND_SYMBOL).count()
    };
    assert!(wait_until(10, || held(&observer.snapshot()) == 2), "the accepted bracket's exits");
    let snap = observer.snapshot();
    assert_eq!(held(&snap), 2, "ONE bracket reached the core — none of the three refused ones");
    assert_eq!(
        snap.orders.iter().filter(|o| o.qty > 10.0 && o.side == 1).count(),
        1,
        "one entry booked on any engine, and it is the accepted bracket's: {:?}",
        snap.orders.iter().map(|o| (&o.client_order_id, &o.symbol, o.qty)).collect::<Vec<_>>()
    );
    mount.handle.shutdown_and_join();
}

/// Send `cmd` as a PREVIEW and then as a COMMAND on one control connection, and return the two
/// refusals — the preview's reason and the command's `Response::Error` — panicking if either was
/// accepted. A refused command books nothing, so the pair leaves the node as it found it.
fn preview_then_command_refusals(
    ctl: &mut TcpStream,
    label: &str,
    cmd: WireCommand,
) -> (String, String) {
    write_frame(ctl, &Request::Preview(cmd.clone())).expect("preview");
    let previewed = match read_frame::<_, Response>(ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(!accepted, "{label} previews as NOT accepted; reason {reason:?}");
            reason.expect("a refusal carries its reason")
        }
        other => panic!("{label}: expected Preview, got {other:?}"),
    };
    write_frame(ctl, &command(cmd)).expect("command");
    let sent = match read_frame::<_, Response>(ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!("{label} must be REFUSED BEFORE THE ACK, got {other:?}"),
    };
    (previewed, sent)
}

/// ⚠ **The DRY RUN refuses a bracket exactly as the send does — the same SENTENCE, not merely
/// a refusal.** `handle_connection`'s preview arm runs the gates itself rather than through
/// `accept_command`; what keeps the two from drifting is that both call `server`'s
/// `bracket_refusal`, the bracket's one verdict. Without the preview's fourth step each of these
/// previewed `accepted` while its send came back an error. The well-formed bracket still previews
/// as accepted, so the step is not refusing every bracket.
#[test]
fn the_dry_run_reports_the_same_bracket_refusals() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-preview");
    let (mut ctl, _features) = control_stream(addr);

    for (label, cmd, needle) in unhonourable_brackets() {
        let (previewed, sent) = preview_then_command_refusals(&mut ctl, label, cmd);
        assert!(previewed.contains(needle), "{label}: {previewed}");
        assert_eq!(previewed, sent, "{label}: the Preview and the Command give ONE sentence");
    }

    write_frame(&mut ctl, &Request::Preview(bracket_on(SECOND_VENUE, SECOND_SYMBOL)))
        .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "the well-formed bracket previews as accepted; reason: {reason:?}")
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// ⚠ **A bracket to a binance engine MOUNTED ON THE SPOT LANE is refused, whatever symbol it
/// names — and above all when it names the `.P` the old refusal told the operator to use.** This is
/// the shipped daemon's shape (its binance engine is spot), driven end to end. The adapter picks
/// its lane once, from the engine's own symbol, and signs every order on it: a `.P` bracket the
/// node admitted would go out on the SPOT lane, its entry would fill, and its stop-loss would be
/// rejected at release, leaving the position protected by its take-profit alone. A paper engine
/// cannot show that last step (it fills stops on either lane), so what this proves is the half
/// that matters on a live box: the node refuses BEFORE THE ACK, the Preview says the same sentence,
/// the sentence names what the engine trades, and nothing is booked.
#[test]
fn a_bracket_to_a_spot_mounted_binance_engine_is_refused_whatever_symbol_it_names() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_spot_binance_node("bracket-spot-engine");
    let (mut ctl, _features) = control_stream(addr);

    // The `.P` FIRST: that is the frame the old check admitted.
    for symbol in [SECOND_SYMBOL, SECOND_SPOT_SYMBOL] {
        let label = format!("a bracket on `{symbol}` to the spot-mounted engine");
        let (previewed, sent) =
            preview_then_command_refusals(&mut ctl, &label, bracket_on(SECOND_VENUE, symbol));
        assert!(sent.contains("spot lane"), "{label}: names the lane: {sent}");
        assert!(sent.contains("trades `VR_ROUTE_BTC`"), "{label}: names what it trades: {sent}");
        assert_eq!(previewed, sent, "{label}: the Preview and the Command give ONE sentence");
    }

    // A plain order on the engine's own symbol, so the absence below is a real absence and not an
    // un-drained snapshot: once it has booked, everything sent before it has been folded.
    write_frame(
        &mut ctl,
        &command(resting_submit("vr-spot-control", SECOND_VENUE, SECOND_SPOT_SYMBOL)),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "vr-spot-control"),
        other => panic!("the plain control order must be Acked, got {other:?}"),
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || observer
            .snapshot()
            .orders
            .iter()
            .any(|o| o.client_order_id == "vr-spot-control")),
        "the control order must book"
    );
    let snap = observer.snapshot();
    assert!(snap.held_exits.is_empty(), "no exit is held: {:?}", snap.held_exits);
    let large: Vec<_> = snap.orders.iter().filter(|o| o.qty > 10.0).collect();
    assert!(
        large.len() == 1 && large[0].client_order_id == "vr-spot-control",
        "no bracket leg was booked on any engine — only the control order: {:?}",
        large.iter().map(|o| (&o.client_order_id, &o.symbol, o.qty)).collect::<Vec<_>>()
    );
    mount.handle.shutdown_and_join();
}

/// ⚠ **The one verb an EMPTY roster refuses, over the wire.** Before the core's first publish this
/// node cannot tell whether a venue runs exactly one account, so a bracket is refused — while a
/// `Submit` on the same connection is still Acked (every other verb reads the empty roster as
/// UNKNOWN, the deadlock rule). Once the node has published, the same bracket is Acked.
#[test]
fn a_bracket_is_refused_before_the_first_publish_and_accepted_after_it() {
    vike_log::test_init();
    let (mount, _halt_dir, publisher, addr) = spawn_unpublished_two_venue_node("bracket-empty");
    let (mut ctl, _features) = control_stream(addr);
    assert!(
        publisher.engine_route_keys().is_empty(),
        "precondition: no bar was driven, so the core has published nothing yet"
    );

    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => assert!(msg.contains("not published"), "{msg}"),
        other => panic!("a bracket on an unpublished roster must be REFUSED, got {other:?}"),
    }

    write_frame(&mut ctl, &command(resting_submit("vr-empty", SECOND_VENUE, SECOND_SYMBOL)))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "vr-empty"),
        other => panic!("every other verb reads an empty roster as UNKNOWN, got {other:?}"),
    }

    publish_both_engines(&mount, &publisher);
    write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, ""),
        other => panic!("after the first publish the bracket must be Acked, got {other:?}"),
    }
    mount.handle.shutdown_and_join();
}

/// ⚠ **DECLARED RESIDUAL, pinned so it cannot change silently: a bracket frame delivered twice is
/// TWO brackets.** Nothing on the wire names a bracket (`BracketSpec` has no id), so the node has
/// nothing to dedup by. A `Submit` re-sent with its id would be refused by the venue. The guard is
/// the client's: a `Disconnected` command is never re-sent. When a bracket gains a client-minted id,
/// this test flips.
#[test]
fn a_bracket_frame_delivered_twice_is_two_brackets() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-twice");
    let (mut ctl, _features) = control_stream(addr);
    for _ in 0..2 {
        write_frame(&mut ctl, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("ack") {
            Response::Ack { coid } => assert_eq!(coid, ""),
            other => panic!("expected Ack, got {other:?}"),
        }
    }
    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || observer
            .snapshot()
            .held_exits
            .iter()
            .filter(|h| h.symbol == SECOND_SYMBOL)
            .count()
            == 4),
        "two brackets, four held exits"
    );
    let parents: std::collections::BTreeSet<String> = observer
        .snapshot()
        .held_exits
        .iter()
        .filter(|h| h.symbol == SECOND_SYMBOL)
        .filter_map(|h| h.parent_order_id.clone())
        .collect();
    assert_eq!(parents.len(), 2, "two distinct entries: {parents:?}");
    mount.handle.shutdown_and_join();
}

/// **An Observe peer cannot place a bracket.** A bracket rides `Request::Command`, so the scope gate
/// that refuses an Observe `Submit` refuses it too, with nothing new to build.
#[test]
fn an_observe_peer_cannot_place_a_bracket() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("bracket-observe");
    let mut stream = TcpStream::connect(addr).expect("connect");
    let (nonce, _features) = hello_welcome(&mut stream);
    let mac = auth::sign(OBSERVE_KEY, &nonce, NODE_PROTO_VERSION, Scope::Read);
    write_frame(&mut stream, &Request::Auth { scope: Scope::Read, mac }).expect("auth");
    match read_frame::<_, Response>(&mut stream).expect("authok") {
        Response::AuthOk { scope: Scope::Read } => {}
        other => panic!("expected AuthOk(Observe), got {other:?}"),
    }
    write_frame(&mut stream, &command(bracket_on(SECOND_VENUE, SECOND_SYMBOL))).expect("command");
    match read_frame::<_, Response>(&mut stream).expect("reply") {
        Response::AuthDenied { reason } => assert!(reason.contains("read-only"), "{reason}"),
        other => panic!("a bracket under Observe must be AuthDenied, got {other:?}"),
    }
    mount.handle.shutdown_and_join();
}
