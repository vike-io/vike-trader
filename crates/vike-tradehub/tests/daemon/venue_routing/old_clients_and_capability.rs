//! Old clients (unaddressed and venue-less wire bytes), the advertised capability and the dry-run.

use super::*;

// ---------------------------------------------------------------------------------------------
// Old clients
// ---------------------------------------------------------------------------------------------

/// **An UNADDRESSED command still reaches today's engine set** — driven from wire BYTES carrying no
/// venue, not from a Rust struct holding a `None`.
///
/// `MassCancel` is the shape that carries the claim: its `venue` is the wire's own "no venue" and
/// the JSON below is what a client that names none actually writes. It fans out across every
/// engine exactly as it did before this gate existed, because `addressed_venue` answers `None` and
/// the gate never sees it.
#[test]
fn an_unaddressed_command_from_the_wire_is_accepted_unchanged() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("unaddressed");
    let (mut ctl, _features) = control_stream(addr);

    let reply = send_raw(
        &mut ctl,
        serde_json::json!({
            "Command": { "cmd": { "MassCancel": { "venue": null, "symbol": null } }, "reason": null }
        }),
    );
    match reply {
        Response::Ack { coid } => assert!(coid.is_empty(), "account-wide verbs echo no coid"),
        other => panic!("an unaddressed MassCancel must still be accepted, got {other:?}"),
    }

    // ...and the same for the account-wide kill switch, whose variant has no venue FIELD at all.
    let reply = send_raw(
        &mut ctl,
        serde_json::json!({
            "Command": { "cmd": { "SetTradingState": "Halted" }, "reason": null }
        }),
    );
    match reply {
        Response::Ack { coid } => assert!(coid.is_empty()),
        other => panic!("SetTradingState names no venue and must be accepted, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// **There has never BEEN an unaddressed submit on this wire**, and this is where that is written
/// down rather than assumed: a `Submit` body with the `venue` key absent does not decode, because
/// `WireOrderRequest::venue` is a plain `String` with no `#[serde(default)]`.
///
/// It matters for the compatibility argument. "An old client sends no venue" is the usual shape of
/// a routing fix's back-compat worry, and here it is IMPOSSIBLE — every client that has ever
/// submitted an order named a venue, which is why the fix is a check on a field rather than a new
/// field, and why no `NODE_PROTO_VERSION` bump is involved.
#[test]
fn a_submit_with_no_venue_key_has_never_decoded() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("novenue");
    let (mut ctl, _features) = control_stream(addr);

    let reply = send_raw(
        &mut ctl,
        serde_json::json!({
            "Command": {
                "cmd": { "Submit": {
                    "client_order_id": "vr-keyless",
                    "symbol": PRIMARY_SYMBOL,
                    "side": 1,
                    "qty": 1.0,
                    "order_type": "limit",
                    "price": 0.10
                }},
                "reason": null
            }
        }),
    );
    match reply {
        Response::Error(msg) => assert!(
            msg.contains("undecodable") || msg.contains("venue"),
            "the frame is refused at DECODE, before any gate: {msg}"
        ),
        other => panic!("a venue-less Submit body must not decode, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

// ---------------------------------------------------------------------------------------------
// The capability, and the dry-run
// ---------------------------------------------------------------------------------------------

/// The node ADVERTISES that it checks the address. Without this string a client cannot tell this
/// node from one that accepts every venue and applies it to its primary book — both answer `Ack`,
/// and the difference shows up only on a venue's statement.
#[test]
fn the_node_advertises_that_it_routes_by_the_addressed_venue() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("feature");
    let (_ctl, features) = control_stream(addr);
    assert!(
        features.iter().any(|f| f == FEATURE_VENUE_ROUTING),
        "Welcome.features must carry `{FEATURE_VENUE_ROUTING}`: {features:?}"
    );
    mount.handle.shutdown_and_join();
}

/// The DRY-RUN answers the same routing verdict the real command would, and still executes
/// nothing. A preview whose whole job is "what would this do" but that is silent about the order
/// landing on a book it does not name is worse than no preview at all.
#[test]
fn the_dry_run_reports_the_same_routing_refusal() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("preview");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(&mut ctl, &Request::Preview(resting_submit("vr-dry", UNMOUNTED_VENUE, "ANY")))
        .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(!accepted, "an unmatched venue previews as NOT accepted");
            let reason = reason.expect("a refusal carries its reason");
            assert!(reason.contains(UNMOUNTED_VENUE), "{reason}");
            assert!(reason.contains(SECOND_VENUE), "{reason}");
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    // ...and a correctly-addressed one previews as accepted.
    write_frame(
        &mut ctl,
        &Request::Preview(resting_submit("vr-dry-ok", SECOND_VENUE, SECOND_SYMBOL)),
    )
    .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "a matched venue previews as accepted; reason: {reason:?}")
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}
