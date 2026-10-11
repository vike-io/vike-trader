//! The ACCOUNT gate on the same path: submits, mounts, reducing verbs and their dry-runs.

use super::*;

// ---------------------------------------------------------------------------------------------
// WHICH BOOK OF IT — the ACCOUNT gate, on the same path
// ---------------------------------------------------------------------------------------------

/// ⚠ **THE WIRING, which is the half a unit test cannot reach.** `server::refusal::account_refusal` has its
/// own pure-verdict suite (`server/tests.rs`'s `account_refusal`) and a mutation proof that the
/// FUNCTION works. Neither says the gate is ON THE PATH: a refactor that dropped
/// `accept_command`'s step 1c, or a surface that passed an EMPTY route-key roster, would leave
/// every one of those tests green and restore the defect in full. This test is that claim, over a
/// real handshake, a real `accept_command` and a real published roster.
///
/// # What the fixture gives, and why a two-ACCOUNT core is not needed to prove it
///
/// This daemon runs ONE account of each venue, so its published route keys are the bare venue ids
/// (`route_key_of` renders the default account that way). A submit naming `ALT` on `binance`
/// therefore composes `binance#ALT`, which no engine here carries — and that is precisely the
/// spec's opening symptom, `binance/NOSUCH` on a node that does not run `NOSUCH`. It used to be
/// Acked and refused out of band by the core, so the client printed `accepted` over an order that
/// never existed.
///
/// ⚠ **The VENUE gate must not be what answers**, and the negative assertion is what makes this
/// test discriminate rather than merely pass: `binance` IS a venue this node runs, so
/// `venue_refusal` is silent on this frame and its sentence must be absent from the reply. A gate
/// that refused here in the venue's words would be the widening that was deliberately not done.
#[test]
fn an_unheld_account_is_refused_over_the_wire_naming_the_accounts_this_node_holds() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("account");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &command(resting_submit_for("vr-alt", SECOND_VENUE, SECOND_SYMBOL, Some("ALT"))),
    )
    .expect("command");
    let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!(
            "a submit naming an account this node does not hold must be REFUSED BEFORE THE ACK — \
             an `Ack` here is the whole defect, got {other:?}"
        ),
    };
    assert!(msg.contains("no account `ALT`"), "the refusal names the account asked for: {msg}");
    assert!(msg.contains(SECOND_VENUE), "...and the venue it was asked for on: {msg}");
    assert!(
        msg.contains("DEFAULT"),
        "...and the account this node DOES hold, under the one spelling a client can send back: \
         {msg}"
    );
    assert!(
        !msg.contains("runs no engine for venue"),
        "⚠ this must NOT be the VENUE gate's sentence — `{SECOND_VENUE}` is a venue this node \
         runs, and answering in those words would be the widening that was deliberately refused: \
         {msg}"
    );

    // ⚠ `DEFAULT` OVER THE WIRE IS ACCEPTED — the ruling, end to end. `route_key_of` renders the
    // unlabelled account as the BARE VENUE ID, so a gate that suffixed the label text would look
    // for `binance#DEFAULT`, match nothing, and refuse the one spelling that addresses the book
    // this node actually has. `vike_core`'s
    // `a_payload_naming_default_reaches_the_unlabelled_account_of_a_two_account_venue` is the same
    // row one plane down.
    write_frame(
        &mut ctl,
        &command(resting_submit_for("vr-default", SECOND_VENUE, SECOND_SYMBOL, Some("DEFAULT"))),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert_eq!(coid, "vr-default"),
        other => panic!("`DEFAULT` NAMES the account this node holds and must be Acked: {other:?}"),
    }

    // ...and a submit naming NO account is byte-identical to before this gate existed. Its case is
    // `vike_core`'s `ambiguous_accounts`, not this gate's, and on a single-account venue there is
    // nothing ambiguous about it.
    write_frame(&mut ctl, &command(resting_submit("vr-bare", SECOND_VENUE, SECOND_SYMBOL)))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert_eq!(coid, "vr-bare"),
        other => panic!("an account-less submit must be unchanged, got {other:?}"),
    }

    let observer = RemoteCoreHandle::connect(addr, OBSERVE_KEY).expect("observe connect");
    assert!(
        wait_until(10, || {
            let s = observer.snapshot();
            ["vr-default", "vr-bare"]
                .iter()
                .all(|c| s.orders.iter().any(|o| &o.client_order_id == c))
        }),
        "the two ACCEPTED orders must book, so the absence below is a real absence rather than an \
         un-drained snapshot"
    );
    assert!(
        !observer.snapshot().orders.iter().any(|o| o.client_order_id == "vr-alt"),
        "the refused order must exist on NO engine — not on the venue it named, not on the \
         primary: {:?}",
        observer
            .snapshot()
            .orders
            .iter()
            .map(|o| (&o.client_order_id, &o.venue))
            .collect::<Vec<_>>()
    );

    mount.handle.shutdown_and_join();
}

/// The DRY-RUN answers the same ACCOUNT verdict, for the venue twin's reason one field along: a
/// preview that is silent about the named BOOK not existing hands the operator a rehearsal whose
/// real send is a refusal. It is a SEPARATE arm in `handle_connection` from the command one, so it
/// is a separate claim — sharing `accept_command` would have made this redundant, and it does not.
#[test]
fn the_dry_run_reports_the_same_account_refusal() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("acct-preview");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &Request::Preview(resting_submit_for(
            "vr-acct-dry",
            SECOND_VENUE,
            SECOND_SYMBOL,
            Some("ALT"),
        )),
    )
    .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(!accepted, "an unheld account previews as NOT accepted");
            let reason = reason.expect("a refusal carries its reason");
            assert!(reason.contains("no account `ALT`"), "{reason}");
            assert!(reason.contains("DEFAULT"), "…naming what this node holds: {reason}");
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    // ...and the held account previews as accepted, so the arm is not simply refusing everything.
    write_frame(
        &mut ctl,
        &Request::Preview(resting_submit_for(
            "vr-acct-dry-ok",
            SECOND_VENUE,
            SECOND_SYMBOL,
            Some("DEFAULT"),
        )),
    )
    .expect("preview");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Preview { accepted, reason } => {
            assert!(accepted, "`DEFAULT` names a held book; reason: {reason:?}")
        }
        other => panic!("expected Preview, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// **THE MOUNT PLANE, over the wire** — the same gate, one rung up the stakes.
///
/// `WireCommand::MountStrategy` names an account too, and until this landed nothing at the edge
/// looked at it: the frame was Acked, `lower_command` validated the SPEC (which says nothing about
/// accounts), and `vike_core`'s `CoreThread::mount_strategy_runtime` refused it on the far side of
/// the single-writer lane as a recent-events note. That is the order plane's defect verbatim,
/// except that what a client hears `accepted` about is not one order but every order the strategy
/// would ever place — `WireCommand::MountStrategy`'s `account` doc is where that argument lives.
///
/// ⚠ **What makes this discriminate rather than merely pass**: in the un-gated state this frame is
/// NOT an error — `buy_hold` with default params is a spec `mount_factory::validate_spec` accepts,
/// so the reply is a plain `Ack` and the refusal arrives nowhere a client is listening. So the
/// failure mode this test is written against is an ACK, not a differently-worded error, and the
/// assertion below names the account to keep a spec refusal from being read as this gate's.
#[test]
fn an_unheld_account_on_a_mount_is_refused_over_the_wire() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("mount-account");
    let (mut ctl, _features) = control_stream(addr);

    write_frame(
        &mut ctl,
        &command(mount_on(SECOND_VENUE, SECOND_SYMBOL, Some("ALT"), "vr-mount-alt")),
    )
    .expect("command");
    let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => msg,
        other => panic!(
            "a MOUNT naming an account this node does not hold must be REFUSED BEFORE THE ACK — \
             an `Ack` here is the whole defect, and it is the one a strategy's entire order flow \
             rides on, got {other:?}"
        ),
    };
    assert!(msg.contains("no account `ALT`"), "the refusal names the account asked for: {msg}");
    assert!(msg.contains(SECOND_VENUE), "...and the venue it was asked for on: {msg}");
    assert!(
        msg.contains("DEFAULT"),
        "...and the account this node DOES hold, under the one spelling a client can send back: \
         {msg}"
    );
    assert!(
        !msg.contains("runs no engine for venue"),
        "⚠ this must NOT be the VENUE gate's sentence — `{SECOND_VENUE}` is a venue this node \
         runs: {msg}"
    );

    // ...and a mount naming the account this node DOES hold is ACCEPTED at the edge, so the arm is
    // not simply refusing every mount. The account-wide-verb convention is an empty echoed coid.
    write_frame(
        &mut ctl,
        &command(mount_on(SECOND_VENUE, SECOND_SYMBOL, Some("DEFAULT"), "vr-mount-default")),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert!(coid.is_empty(), "mount verbs echo no coid"),
        other => panic!("`DEFAULT` NAMES the account this node holds and must be Acked: {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// **THE REDUCING PLANE, over the wire** — owner ruling "B", 2026-09-26: a mass-cancel, a flatten
/// or a market exit naming an account this node does not hold is REFUSED BEFORE THE ACK.
///
/// Until this, all three were Acked whatever account they named: `lower_command` dropped the field
/// and the core fanned the verb over every account of the venue, so on a two-account box
/// `market-exit binance ALT` cancelled and flattened the DEFAULT account too. The core narrows now,
/// and a named account it does not hold is exactly the out-of-band refusal this edge exists to move
/// in front of the Ack — so the failure this is written against is an `Ack`, as on the mount plane.
///
/// This node runs ONE account of `binance`, so `ALT` is unheld and `DEFAULT` is the account it
/// holds; the roster in the refusal must say so under the one spelling a client can send back.
#[test]
fn an_unheld_account_on_a_reducing_verb_is_refused_over_the_wire() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("reduce-account");
    let (mut ctl, features) = control_stream(addr);
    assert!(
        features.iter().any(|f| f == FEATURE_ACCOUNT_SCOPED_REDUCE),
        "the node honours a labelled reduce, so it must advertise it: {features:?}"
    );

    let reducers = |account: &str| {
        let account = Some(account.to_string());
        vec![
            (
                "The mass-cancel",
                WireCommand::MassCancel {
                    venue: Some(SECOND_VENUE.into()),
                    symbol: None,
                    account: account.clone(),
                },
            ),
            (
                "The flatten",
                WireCommand::Flatten {
                    venue: SECOND_VENUE.into(),
                    symbol: SECOND_SYMBOL.into(),
                    account: account.clone(),
                },
            ),
            (
                "The market exit",
                WireCommand::MarketExit { venue: Some(SECOND_VENUE.into()), account },
            ),
        ]
    };

    for (subject, cmd) in reducers("ALT") {
        write_frame(&mut ctl, &command(cmd)).expect("command");
        let msg = match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Error(msg) => msg,
            other => panic!(
                "{subject} naming an account this node does not hold must be REFUSED BEFORE THE \
                 ACK — an `Ack` here is the whole defect, got {other:?}"
            ),
        };
        assert!(msg.contains("no account `ALT`"), "{subject}: names the account: {msg}");
        assert!(msg.contains(SECOND_VENUE), "{subject}: …and the venue: {msg}");
        assert!(msg.contains("DEFAULT"), "{subject}: …and the account this node holds: {msg}");
        assert!(msg.contains(&format!("{subject} was REFUSED")), "{subject}: its subject: {msg}");
        assert!(
            !msg.contains("runs no engine for venue"),
            "{subject}: ⚠ NOT the venue gate's sentence — `{SECOND_VENUE}` is run here: {msg}"
        );
    }

    // …and the account this node DOES hold is accepted on all three — the arm is not simply
    // refusing every labelled reduce. The account-wide convention: an empty echoed coid.
    for (subject, cmd) in reducers("DEFAULT") {
        write_frame(&mut ctl, &command(cmd)).expect("command");
        match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Ack { coid } => assert!(coid.is_empty(), "{subject}: echoes no coid"),
            other => panic!("{subject} naming `DEFAULT` names a held book: {other:?}"),
        }
    }

    // ⚠ An account with NO venue is refused, never widened into the global exit it would
    // otherwise reach.
    write_frame(
        &mut ctl,
        &command(WireCommand::MarketExit { venue: None, account: Some("ALT".into()) }),
    )
    .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Error(msg) => {
            assert!(msg.contains("`ALT`") && msg.contains("no venue"), "names both: {msg}")
        }
        other => panic!("an account with no venue must be refused, got {other:?}"),
    }

    // ⚠ …and the UNSCOPED panic button, on the same connection, is still Acked.
    write_frame(&mut ctl, &command(WireCommand::MarketExit { venue: None, account: None }))
        .expect("command");
    match read_frame::<_, Response>(&mut ctl).expect("reply") {
        Response::Ack { coid } => assert!(coid.is_empty(), "account-wide verbs echo no coid"),
        other => panic!("the UNSCOPED panic button must never be refused, got {other:?}"),
    }

    mount.handle.shutdown_and_join();
}

/// The DRY-RUN answers the same reducing-plane verdicts, because `handle_connection`'s preview arm
/// runs the gates itself rather than through `accept_command`: an unheld account and an account
/// with no venue both preview as NOT accepted, and the held account previews as accepted.
#[test]
fn the_dry_run_reports_the_same_reducing_verb_refusals() {
    vike_log::test_init();
    let (mount, _halt_dir, _publisher, addr) = spawn_two_venue_node("reduce-preview");
    let (mut ctl, _features) = control_stream(addr);

    for (cmd, want_accepted, needle) in [
        (
            WireCommand::MarketExit {
                venue: Some(SECOND_VENUE.into()),
                account: Some("ALT".into()),
            },
            false,
            "no account `ALT`",
        ),
        (WireCommand::MarketExit { venue: None, account: Some("ALT".into()) }, false, "no venue"),
        (
            WireCommand::MarketExit {
                venue: Some(SECOND_VENUE.into()),
                account: Some("DEFAULT".into()),
            },
            true,
            "",
        ),
    ] {
        let label = format!("{cmd:?}");
        write_frame(&mut ctl, &Request::Preview(cmd)).expect("preview");
        match read_frame::<_, Response>(&mut ctl).expect("reply") {
            Response::Preview { accepted, reason } => {
                assert_eq!(accepted, want_accepted, "{label}: reason {reason:?}");
                if !want_accepted {
                    let reason = reason.expect("a refusal carries its reason");
                    assert!(reason.contains(needle), "{label}: {reason}");
                }
            }
            other => panic!("expected Preview, got {other:?}"),
        }
    }

    mount.handle.shutdown_and_join();
}
