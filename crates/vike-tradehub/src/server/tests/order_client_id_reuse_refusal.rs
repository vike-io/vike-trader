//! `order_client_id_reuse_refusal` — the door's answer to a `Submit` whose order client id an order of this
//! node already carries. The symptom it closes (measured 2026-10-26, FXCM account 9 and Dukascopy):
//! `trade order submit … --order-client-id <the id of a FILLED order>` printed `accepted by the node`, the
//! repeat never reached the venue, and the stuck-order watchdog rejected the half-created order
//! 30 s later with a reason that named nothing.

use std::collections::HashSet;
use std::sync::Arc;

use super::refusal::order_client_id_reuse_refusal;
use vike_model::OrderStatus;
use vike_model::orders::client_id::{
    OrderClientIdAccountCode, OrderClientIdGenerator, OrderClientIdIntent, OrderClientIdLegRole,
    OrderClientIdPlan, OrderClientIdShape, OrderClientIdSource, OrderClientIdStrategyCode,
};
use vike_tradehub_client::wire::{
    CommandWire, OrderBracketWire, OrderComboWire, OrderListLegWire, OrderListWire,
    OrderRequestWire,
};

/// No late-retry memory: a node with no journal, or a surface never handed one.
fn none() -> HashSet<String> {
    HashSet::new()
}

/// The sentence every door says of an id the late-retry memory holds (the core's `refuse_recent_order_client_id` too).
fn late(order_client_id: &str) -> String {
    format!(
        "order client id {order_client_id} was used by an order before this node's last restart (within 24 h): pick a new \
         order client id"
    )
}

fn list_with(order_client_ids: &[&str]) -> CommandWire {
    CommandWire::OrderList(OrderListWire {
        venue: "binance".into(),
        symbol: "SYM".into(),
        account: None,
        legs: order_client_ids
            .iter()
            .map(|c| OrderListLegWire {
                side: 1,
                qty: 1.0,
                order_type: "limit".into(),
                price: Some(1.0),
                trigger_price: None,
                trail: None,
                reduce_only: false,
                time_in_force: None,
                parent: None,
                contingency: None,
                linked: Vec::new(),
                order_client_id: Some((*c).to_string()),
            })
            .collect(),
    })
}

fn combo_as(order_client_id: &str) -> CommandWire {
    let leg = |symbol: &str, ratio| vike_model::OrderComboLeg {
        symbol: symbol.into(),
        ratio,
        limit: None,
    };
    CommandWire::Combo(OrderComboWire {
        venue: "deribit".into(),
        side: 1,
        qty: 1.0,
        legs: vec![leg("A-C", 1), leg("B-C", -1)],
        net_limit: Some(0.01),
        account: None,
        time_in_force: None,
        order_client_id: Some(order_client_id.into()),
    })
}

fn bracket_with(stem: Option<&str>) -> CommandWire {
    CommandWire::Bracket(OrderBracketWire {
        venue: "binance".into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        entry_price: Some(1.0),
        stop_loss: 0.9,
        take_profit: 1.1,
        account: None,
        order_client_id: stem.map(str::to_string),
    })
}

/// A real v2 group stem, as a GUI mints one for account #4.
fn gui_stem() -> String {
    let plan = OrderClientIdPlan::new(OrderClientIdShape::FULL, OrderClientIdAccountCode::of_id(4));
    let mut order_client_ids = OrderClientIdGenerator::new_v2(None);
    let stem = plan.generate_stem(
        &mut order_client_ids,
        OrderClientIdSource::Gui,
        OrderClientIdStrategyCode::NONE,
    );
    stem.as_str().expect("a v2 session's stem").to_string()
}

fn submit_as(order_client_id: &str) -> CommandWire {
    CommandWire::Submit(OrderRequestWire {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "SYM".into(),
        side: 1,
        qty: 1.0,
        order_type: "limit".into(),
        price: Some(1.0),
        trigger_price: None,
        reduce_only: false,
        account: None,
        time_in_force: None,
        gtd_expiry: None,
        display_quantity: None,
        release: None,
        trail: None,
        limit_offset: None,
        peg: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
    })
}

fn order(order_client_id: &str, status: OrderStatus) -> vike_execution::OrderView {
    vike_execution::OrderView {
        data_delay: None,
        order_client_id: order_client_id.into(),
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
        average_fill_price: 0.0,
        reason: None,
        pending_price: None,
        trail: None,
        limit_offset: None,
        attributes: None,
        trail_percent: None,
        adjust: None,
        release: None,
        display_quantity: None,
    }
}

/// An order client id that a FINISHED order carries is refused in the sentence the operator can act on, for every
/// terminal status (a rejected order keeps its id too: the registry holds it for the life of the node).
///
/// ⚠ KILL PROOF: delete the `order_client_id_reuse_refusal` call from `accept_command` and the production-path
/// test below finds an accepted command; return `None` from this function and every row here fails.
#[test]
fn a_submit_of_a_terminal_order_client_id_is_refused_in_words() {
    for status in [
        OrderStatus::Filled,
        OrderStatus::Canceled,
        OrderStatus::Rejected,
        OrderStatus::Expired,
        OrderStatus::Denied,
    ] {
        let msg = order_client_id_reuse_refusal(
            &submit_as("b93fxent"),
            &[order("b93fxent", status)],
            &none(),
        )
        .unwrap_or_else(|| panic!("{status:?} must be refused"));
        assert_eq!(
            msg,
            format!(
                "order client id b93fxent is already used by an order that is {}: pick a new order client id",
                status.as_str()
            )
        );
    }
}

/// An order client id that a LIVE order carries is refused too, and the sentence says the order is still live: an
/// echo of `accepted` would be the same lie, and the registry entry of a resting order must never be
/// replaced by a fresh `INITIALIZED` one.
#[test]
fn a_submit_of_a_live_order_client_id_is_refused_and_says_the_order_is_live() {
    for status in [
        OrderStatus::Initialized,
        OrderStatus::Submitted,
        OrderStatus::Accepted,
        OrderStatus::PartiallyFilled,
        OrderStatus::PendingCancel,
    ] {
        let msg =
            order_client_id_reuse_refusal(&submit_as("rest1"), &[order("rest1", status)], &none())
                .unwrap_or_else(|| panic!("{status:?} must be refused"));
        assert!(
            msg.starts_with(&format!(
                "order client id rest1 is already used by an order that is {} (still live): pick a new order \
                 client id",
                status.as_str()
            )),
            "{msg}"
        );
        assert!(msg.contains("modify or cancel"), "it says how to change the live order: {msg}");
    }
}

/// What the gate must NOT touch: an order client id no order carries (a first submit, and the retry of a command the
/// node never received), a different order client id, and every verb that is not a `Submit` (a `Modify` and a
/// `Cancel` name the order client id on purpose).
#[test]
fn a_free_order_client_id_and_every_other_verb_pass() {
    let filled = [order("done", OrderStatus::Filled)];
    assert_eq!(
        order_client_id_reuse_refusal(&submit_as("fresh"), &[], &none()),
        None,
        "nothing published yet"
    );
    assert_eq!(
        order_client_id_reuse_refusal(&submit_as("fresh"), &filled, &none()),
        None,
        "another id"
    );
    let modify = CommandWire::Modify {
        order_client_id: "done".into(),
        new_qty: Some(1.0),
        new_price: Some(1.0),
        new_trigger_price: None,
    };
    assert_eq!(
        order_client_id_reuse_refusal(&modify, &filled, &none()),
        None,
        "a modify names the id on purpose"
    );
    assert_eq!(
        order_client_id_reuse_refusal(&CommandWire::Cancel("done".into()), &filled, &none()),
        None
    );
}

/// The gate is reached THROUGH `accept_command`, the production call the table above bypasses, and it is
/// the only thing that refuses: the same command is accepted when the snapshot holds no such order. The
/// paper core exists only to supply a real `CommandSink`. The refusal is a `Refused`, which leaves the
/// surface open (a typo'd id is the operator's to retype, never a reason to close the connection).
#[test]
fn accept_command_refuses_a_filled_order_client_id_before_the_ack_and_a_new_one_goes() {
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategies_check::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        "ORDER_CLIENT_ID_REUSE_TOKEN",
        Some(3_000_000_000),
    );
    let sink = mount.handle.command_sink();
    let mut limits = super::control::ControlLimits::new(super::control::ControlLimitsConfig {
        max_notional: None,
        rate_per_sec: 1e9,
    });
    let mut accept = |cmd: CommandWire, orders: &[vike_execution::OrderView]| {
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
        "order client id b93fxent is already used by an order that is FILLED: pick a new order client id"
    );
    assert!(!err.is_fatal(), "a refusal never closes the surface");

    assert!(accept(submit_as("b93fxent"), &[]).is_ok(), "the same command, with no such order");
    assert!(accept(submit_as("b93fxen2"), &filled).is_ok(), "a new id goes");
    mount.handle.shutdown_and_join();
}

/// **An id used before this node's last restart is refused in words, per verb** (decision 0270, D7): the registry
/// no longer holds it, the late-retry memory does. A submit's id, any leg of an order list, a combo's id and any leg a
/// bracket's stem derives (every intent: the position the intent is read from may have moved) are refused, a list,
/// a combo and a bracket WHOLE; a new id, a bracket the node mints the ids of, and a text that is no stem pass (the
/// core says what is wrong with the last). An order the snapshot holds is still said by its status first.
///
/// ⚠ KILL PROOF: drop the memory arm from `order_client_id_reuse_refusal` and every refusal below goes `None`.
#[test]
fn an_id_used_before_the_last_restart_is_refused_in_words_per_verb() {
    let stem = gui_stem();
    let leg = |intent, role| {
        vike_model::orders::client_id::OrderClientIdStem::given(&stem).expect("a v2 stem").leg(
            &mut OrderClientIdGenerator::resume(String::new(), 0),
            intent,
            role,
        )
    };
    let stop = leg(OrderClientIdIntent::CloseLong, OrderClientIdLegRole::Stop);
    let recent: HashSet<String> =
        ["late1", "lateleg", "latecombo", stop.as_str()].into_iter().map(str::to_string).collect();

    assert_eq!(
        order_client_id_reuse_refusal(&submit_as("late1"), &[], &recent),
        Some(late("late1"))
    );
    assert_eq!(
        order_client_id_reuse_refusal(&list_with(&["fresh0", "lateleg"]), &[], &recent),
        Some(format!(
            "order list: {}. The order list was REFUSED whole and no leg of it was sent",
            late("lateleg")
        ))
    );
    assert_eq!(
        order_client_id_reuse_refusal(&combo_as("latecombo"), &[], &recent),
        Some(format!(
            "combo: {}. The combo was REFUSED whole and nothing of it was sent",
            late("latecombo")
        ))
    );
    assert_eq!(
        order_client_id_reuse_refusal(&bracket_with(Some(&stem)), &[], &recent),
        Some(format!(
            "bracket: {}. The bracket was REFUSED whole and nothing of it was sent",
            late(&stop)
        ))
    );
    // Every role, under every intent, is a leg of the stem.
    for role in OrderClientIdLegRole::BRACKET {
        for intent in OrderClientIdIntent::ALL {
            let only: HashSet<String> = [leg(intent, role)].into_iter().collect();
            assert!(
                order_client_id_reuse_refusal(&bracket_with(Some(&stem)), &[], &only).is_some(),
                "{role:?} {intent:?}"
            );
        }
    }

    // What passes: new ids, a bracket the node mints, a text that is no stem.
    assert_eq!(order_client_id_reuse_refusal(&submit_as("fresh1"), &[], &recent), None);
    assert_eq!(
        order_client_id_reuse_refusal(&list_with(&["fresh0", "fresh1"]), &[], &recent),
        None
    );
    assert_eq!(order_client_id_reuse_refusal(&combo_as("freshcombo"), &[], &recent), None);
    assert_eq!(order_client_id_reuse_refusal(&bracket_with(Some(&gui_stem())), &[], &recent), None);
    assert_eq!(order_client_id_reuse_refusal(&bracket_with(None), &[], &recent), None);
    assert_eq!(order_client_id_reuse_refusal(&bracket_with(Some("late1")), &[], &recent), None);
    // An order the snapshot holds is said by its status, ahead of the memory.
    assert_eq!(
        order_client_id_reuse_refusal(&submit_as("late1"), &[order("late1", OrderStatus::Filled)], &recent),
        Some(
            "order client id late1 is already used by an order that is FILLED: pick a new order client id"
                .into()
        )
    );
}

/// **The door is HANDED the orders an order list, a combo and a bracket name** (`crate::publish::named_orders`, the
/// orders `accept_command` and the preview are given): a live OCO leg's, a combo's and a bracket leg's id. So a retried
/// list, combo or bracket is refused whole, and a new submit reusing any of those ids is refused, all before the `Ack`;
/// new ids are handed nothing.
///
/// ⚠ KILL PROOF: drop the new `named_orders` arm and the three group retries below are handed no order and pass.
#[test]
fn the_door_sees_the_live_orders_a_list_a_combo_and_a_bracket_name() {
    let stem = gui_stem();
    let bracket_leg =
        vike_model::orders::client_id::OrderClientIdStem::given(&stem).expect("a v2 stem").leg(
            &mut OrderClientIdGenerator::resume(String::new(), 0),
            OrderClientIdIntent::OpenLong,
            OrderClientIdLegRole::Entry,
        );
    let published = [
        order("oco1", OrderStatus::Accepted),
        order("cmb1", OrderStatus::Submitted),
        order(&bracket_leg, OrderStatus::Accepted),
        order("other", OrderStatus::Accepted),
    ];
    let door = |cmd: &CommandWire| {
        let named = crate::publish::named_orders(cmd, &published);
        order_client_id_reuse_refusal(cmd, &named, &none())
    };
    let live = |order_client_id: &str| {
        format!(
            "order client id {order_client_id} is already used by an order that is ACCEPTED (still live): pick a new \
             order client id, or change that order by this id with modify or cancel"
        )
    };
    assert_eq!(
        door(&list_with(&["fresh0", "oco1"])),
        Some(format!(
            "order list: {}. The order list was REFUSED whole and no leg of it was sent",
            live("oco1")
        ))
    );
    assert!(
        door(&combo_as("cmb1"))
            .is_some_and(|s| s.starts_with("combo: order client id cmb1 is already"))
    );
    assert_eq!(
        door(&bracket_with(Some(&stem))),
        Some(format!(
            "bracket: {}. The bracket was REFUSED whole and nothing of it was sent",
            live(&bracket_leg)
        ))
    );
    for reused in ["oco1", "cmb1", bracket_leg.as_str()] {
        assert!(door(&submit_as(reused)).is_some(), "a submit reusing {reused}");
    }
    assert!(crate::publish::named_orders(&list_with(&["a", "b"]), &published).is_empty());
    assert_eq!(door(&list_with(&["fresh0", "fresh1"])), None);
    assert_eq!(door(&combo_as("freshcombo")), None);
    assert_eq!(door(&bracket_with(Some(&gui_stem()))), None);
    assert_eq!(door(&bracket_with(None)), None);
}

/// **The memory is consulted THROUGH `accept_command`**, as the surface hands it in (`ControlLimits::with_boot_order_client_ids`,
/// shared read-only): a late retry is refused before the `Ack`, the surface stays open, a new id goes, and a surface
/// never handed the memory refuses nothing on that ground.
#[test]
fn accept_command_refuses_a_late_retry_before_the_ack_and_a_new_id_goes() {
    let mount = vike_tradehub_core::test_support::paper_maker::paper_maker_core(
        vike_strategies_check::makers::spread_maker::SpreadMaker::from_params,
        "polymarket",
        "ORDER_CLIENT_ID_LATE_TOKEN",
        Some(3_000_000_000),
    );
    let sink = mount.handle.command_sink();
    let config = super::control::ControlLimitsConfig { max_notional: None, rate_per_sec: 1e9 };
    let memory = Arc::new(["b93late".to_string()].into_iter().collect::<HashSet<String>>());
    let accept = |limits: &mut super::control::ControlLimits, cmd: CommandWire| {
        super::control::accept_command(
            cmd,
            None,
            limits,
            &sink,
            None,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            None,
            None,
            None,
        )
    };
    let mut remembering =
        super::control::ControlLimits::new(config).with_boot_order_client_ids(memory);
    let err = accept(&mut remembering, submit_as("b93late")).expect_err("used before the restart");
    assert_eq!(err.message(), late("b93late"));
    assert!(!err.is_fatal(), "a refusal never closes the surface");
    assert!(accept(&mut remembering, submit_as("b93new")).is_ok(), "a new id goes");
    let mut forgetting = super::control::ControlLimits::new(config);
    assert!(accept(&mut forgetting, submit_as("b93late")).is_ok(), "no memory handed in");
    mount.handle.shutdown_and_join();
}
