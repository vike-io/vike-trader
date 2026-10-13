//! The VENUE gate: an addressed submit reaches the engine it names, an unmatched address is refused.

use super::*;

// ---------------------------------------------------------------------------------------------
// The assertion that would have caught this
// ---------------------------------------------------------------------------------------------

/// **Two addresses, two books.** Each submit names its own venue and appears in the snapshot owned
/// by THAT venue's engine — `OrderView::venue` is the OWNING ENGINE's venue (`vike_core::snapshot::build`
/// zips each registry with the engine it came from), so this discriminates the engine that handled
/// the order rather than echoing the string the request carried.
///
/// The binance half is the load-bearing one: polymarket is the PRIMARY, so an order that failed to
/// route would land there, and this assertion fails naming the wrong owner.
#[test]
fn each_addressed_submit_reaches_the_engine_it_names() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("reaches");
    let (mut ctl, _features) = control_stream(addr);

    for (coid, venue, symbol) in
        [("vr-primary", PRIMARY_VENUE, PRIMARY_SYMBOL), ("vr-second", SECOND_VENUE, SECOND_SYMBOL)]
    {
        write_frame(&mut ctl, &command(resting_submit(coid, venue, symbol))).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("ack") {
            Response::Ack { coid: echoed } => assert_eq!(echoed, coid),
            other => panic!("expected Ack{{{coid}}}, got {other:?}"),
        }
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || {
            let s = observer.snapshot();
            ["vr-primary", "vr-second"]
                .iter()
                .all(|c| s.orders.iter().any(|o| &o.client_order_id == c))
        }),
        "both control-submitted orders must reach the pushed snapshot"
    );
    let snap = observer.snapshot();
    let owner = |coid: &str| {
        snap.orders
            .iter()
            .find(|o| o.client_order_id == coid)
            .unwrap_or_else(|| panic!("order {coid} present"))
            .venue
            .clone()
    };
    assert_eq!(
        owner("vr-primary"),
        PRIMARY_VENUE,
        "the order naming the primary venue is owned by the primary engine"
    );
    assert_eq!(
        owner("vr-second"),
        SECOND_VENUE,
        "the order naming the SECOND venue must be owned by the SECOND engine — owned by \
         `{PRIMARY_VENUE}` here would be exactly the misroute this suite exists for"
    );

    mount.handle.shutdown_and_join();
}

// ---------------------------------------------------------------------------------------------
// The refusal
// ---------------------------------------------------------------------------------------------

/// **An address this node cannot match is REFUSED, and the refusal names what it does have.**
/// Nothing is booked — on either engine — so the operator's retry is against a node whose state is
/// exactly what it was.
#[test]
fn an_unmatched_address_is_refused_naming_the_venues_this_node_runs() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("unmatched");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(&mut ctl, &command(resting_submit("vr-nowhere", UNMOUNTED_VENUE, "ANY")))
        .expect("command");
    let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!("expected Error for an unmatched venue, got {other:?}"),
    };
    assert!(msg.contains(UNMOUNTED_VENUE), "the refusal names the venue that was asked for: {msg}");
    assert!(msg.contains(PRIMARY_VENUE), "...and the venues this node DOES run: {msg}");
    assert!(msg.contains(SECOND_VENUE), "...both of them: {msg}");

    // The connection SURVIVES a refusal (it is not fatal), so the same peer can immediately send a
    // correctly-addressed command — which is what makes "refusing costs one retry" true rather
    // than merely claimed.
    write_frame(&mut ctl, &command(resting_submit("vr-retry", SECOND_VENUE, SECOND_SYMBOL)))
        .expect("retry");
    match read_frame::<_, Response>(&mut ctl).expect("ack") {
        Response::Ack { coid } => assert_eq!(coid, "vr-retry"),
        other => panic!("expected Ack after the refusal, got {other:?}"),
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || observer
            .snapshot()
            .orders
            .iter()
            .any(|o| o.client_order_id == "vr-retry")),
        "the retry must book (so the absence below is a real absence, not an un-drained snapshot)"
    );
    assert!(
        !observer.snapshot().orders.iter().any(|o| o.client_order_id == "vr-nowhere"),
        "the refused order must exist on NO engine — refusing means refusing, not re-addressing: \
         {:?}",
        observer
            .snapshot()
            .orders
            .iter()
            .map(|o| (&o.client_order_id, &o.venue))
            .collect::<Vec<_>>()
    );

    mount.handle.shutdown_and_join();
}

/// The risk-REDUCING venue verbs are refused on an unmatched address too, and the UNSCOPED panic
/// button never is.
///
/// ⚠ The judgment here is deliberate and goes the same way as the submit: a `market-exit okx`
/// against a node with no okx engine used to reach `exit_scope_engines`' empty-set fallback and act
/// on engine 0 — it would CANCEL the resting orders of a book the operator was not talking about.
/// Refusing costs a retry; acting cancels the wrong quotes. The unscoped form (`venue: None`) names
/// the whole set and is the one shape that must never need an argument, so the gate cannot see it
/// at all (`WireCommand::addressed_venue` answers `None`).
#[test]
fn a_scoped_exit_is_refused_on_an_unmatched_venue_while_the_unscoped_panic_button_is_not() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("exit");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &command(WireCommand::MarketExit {
            venue: Some(UNMOUNTED_VENUE.to_string()),
            account: None,
        }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => {
            assert!(msg.contains(UNMOUNTED_VENUE), "the scoped exit refusal names it: {msg}")
        }
        other => panic!("expected Error for a scoped exit on an unmatched venue, got {other:?}"),
    }

    write_frame(&mut ctl, &command(WireCommand::MarketExit { venue: None, account: None }))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        // The account-wide convention: an empty coid, and an Ack rather than a refusal.
        Response::Ack { coid } => assert!(coid.is_empty(), "account-wide verbs echo no coid"),
        other => panic!(
            "the UNSCOPED panic button must never be refused by the routing gate, got {other:?}"
        ),
    }

    mount.handle.shutdown_and_join();
}
