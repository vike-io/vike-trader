//! `coid_reuse_refusal` — the door's answer to a `Submit` whose client order id an order of this
//! node already carries. The symptom it closes (measured 2026-10-26, FXCM account 9 and Dukascopy):
//! `trade order submit … --coid <the id of a FILLED order>` printed `accepted by the node`, the
//! repeat never reached the venue, and the stuck-order watchdog rejected the half-created order
//! 30 s later with a reason that named nothing.

use super::refusal::coid_reuse_refusal;
use vike_model::OrderStatus;
use vike_tradehub_client::wire::{WireCommand, WireOrderRequest};

fn submit_as(coid: &str) -> WireCommand {
    WireCommand::Submit(WireOrderRequest {
        client_order_id: coid.into(),
        venue: "binance".into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
    })
}

fn order(coid: &str, status: OrderStatus) -> vike_exec::OrderView {
    vike_exec::OrderView {
        data_delay: None,
        client_order_id: coid.into(),
        venue: "binance".into(),
        account: None,
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: vike_model::OrderType::Limit,
        price: Some(1.0),
        trigger_price: None,
        status,
        venue_order_id: None,
        filled_qty: 0.0,
        avg_fill_px: 0.0,
        reason: None,
    }
}

/// A coid that a FINISHED order carries is refused in the sentence the operator can act on, for every
/// terminal status (a rejected order keeps its id too: the registry holds it for the life of the node).
///
/// ⚠ KILL PROOF: delete the `coid_reuse_refusal` call from `accept_command` and the production-path
/// test below finds an accepted command; return `None` from this function and every row here fails.
#[test]
fn a_submit_of_a_terminal_coid_is_refused_in_words() {
    for status in [
        OrderStatus::Filled,
        OrderStatus::Canceled,
        OrderStatus::Rejected,
        OrderStatus::Expired,
        OrderStatus::Denied,
    ] {
        let msg = coid_reuse_refusal(&submit_as("b93fxent"), &[order("b93fxent", status)])
            .unwrap_or_else(|| panic!("{status:?} must be refused"));
        assert_eq!(
            msg,
            format!(
                "coid b93fxent is already used by an order that is {}: pick a new client order id",
                status.as_str()
            )
        );
    }
}

/// A coid that a LIVE order carries is refused too, and the sentence says the order is still live: an
/// echo of `accepted` would be the same lie, and the registry entry of a resting order must never be
/// replaced by a fresh `INITIALIZED` one.
#[test]
fn a_submit_of_a_live_coid_is_refused_and_says_the_order_is_live() {
    for status in [
        OrderStatus::Initialized,
        OrderStatus::Submitted,
        OrderStatus::Accepted,
        OrderStatus::PartiallyFilled,
        OrderStatus::PendingCancel,
    ] {
        let msg = coid_reuse_refusal(&submit_as("rest1"), &[order("rest1", status)])
            .unwrap_or_else(|| panic!("{status:?} must be refused"));
        assert!(
            msg.starts_with(&format!(
                "coid rest1 is already used by an order that is {} (still live): pick a new client \
                 order id",
                status.as_str()
            )),
            "{msg}"
        );
        assert!(msg.contains("modify or cancel"), "it says how to change the live order: {msg}");
    }
}

/// What the gate must NOT touch: a coid no order carries (a first submit, and the retry of a command the
/// node never received), a different coid, and every verb that is not a `Submit` (a `Modify` and a
/// `Cancel` name the coid on purpose).
#[test]
fn a_free_coid_and_every_other_verb_pass() {
    let filled = [order("done", OrderStatus::Filled)];
    assert_eq!(coid_reuse_refusal(&submit_as("fresh"), &[]), None, "nothing published yet");
    assert_eq!(coid_reuse_refusal(&submit_as("fresh"), &filled), None, "another id");
    let modify = WireCommand::Modify {
        client_order_id: "done".into(),
        new_qty: Some(1.0),
        new_price: Some(1.0),
    };
    assert_eq!(coid_reuse_refusal(&modify, &filled), None, "a modify names the id on purpose");
    assert_eq!(coid_reuse_refusal(&WireCommand::Cancel("done".into()), &filled), None);
}

/// The gate is reached THROUGH `accept_command`, the production call the table above bypasses, and it is
/// the only thing that refuses: the same command is accepted when the snapshot holds no such order. The
/// paper core exists only to supply a real `CommandSink`. The refusal is a `Refused`, which leaves the
/// surface open (a typo'd id is the operator's to retype, never a reason to close the connection).
#[test]
fn accept_command_refuses_a_filled_coid_before_the_ack_and_a_new_one_goes() {
    let mount = vike_tradehub_core::build_paper_maker_core(
        &vike_tradehub_core::MakerConfig::outcome_token(
            "polymarket",
            "COID_REUSE_TOKEN",
            Some(3_000_000_000),
        ),
    );
    let sink = mount.handle.command_sink();
    let mut limits = super::control::ControlLimits::new(super::control::ControlLimitsConfig {
        max_notional: None,
        rate_per_sec: 1e9,
    });
    let mut accept = |cmd: WireCommand, orders: &[vike_exec::OrderView]| {
        super::control::accept_command(
            cmd,
            None,
            &mut limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            &[],
            orders,
            &[],
            None,
            None,
            None,
        )
    };

    let filled = [order("b93fxent", OrderStatus::Filled)];
    let err = accept(submit_as("b93fxent"), &filled).expect_err("the id belongs to a FILLED order");
    assert_eq!(
        err.message(),
        "coid b93fxent is already used by an order that is FILLED: pick a new client order id"
    );
    assert!(!err.is_fatal(), "a refusal never closes the surface");

    assert!(accept(submit_as("b93fxent"), &[]).is_ok(), "the same command, with no such order");
    assert!(accept(submit_as("b93fxen2"), &filled).is_ok(), "a new id goes");
    mount.handle.shutdown_and_join();
}
