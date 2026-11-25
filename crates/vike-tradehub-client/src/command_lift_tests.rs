use super::*;
use crate::wire::{CommandWire, OrderBracketWire, OrderRequestWire};
use std::assert_matches;
use vike_model::TradingState;

fn req(order_client_id: &str) -> vike_model::OrderRequest {
    vike_model::OrderRequest {
        order_client_id: order_client_id.into(),
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        side: 1,
        qty: 0.5,
        order_type: vike_model::OrderType::Limit,
        price: Some(59_000.0),
        trigger_price: None,
        reduce_only: true,
        ..Default::default()
    }
}

#[test]
fn submit_copies_all_nine_wire_fields() {
    let cmd =
        vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(req("c-1"))));
    // Compare the WHOLE CommandWire (derived PartialEq) — asserts all 9 carried fields at once
    // AND keeps the float fields off a source-level `==` (which would trip `clippy::float_cmp`).
    assert_eq!(
        wire_from_command(&cmd),
        Some(CommandWire::Submit(OrderRequestWire {
            order_client_id: "c-1".into(),
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            order_type: "limit".into(),
            price: Some(59_000.0),
            trigger_price: None,
            reduce_only: true,
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
        }))
    );
}

/// A Submit's display and release cross the lift, the release's price condition with it: on the order's own symbol
/// with no `symbol` key, on ANOTHER symbol naming it.
#[test]
fn a_submit_carries_its_display_and_its_release() {
    use crate::wire::{OrderReleasePriceWire, OrderReleaseWire};
    use vike_model::{OrderRelease, OrderReleasePrice, OrderReleasePriceSide};
    let mut request = req("c-1");
    request.display_quantity = Some(0.1);
    let on = |symbol: &str| OrderReleasePrice {
        symbol: symbol.into(),
        level: 58_000.0,
        side: OrderReleasePriceSide::Below,
        venue: None,
    };
    request.release = Some(OrderRelease { at: 4_097_217_600_000, price: Some(on("BTCUSDT")) });
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(
        request.clone(),
    )));
    let Some(CommandWire::Submit(w)) = wire_from_command(&cmd) else { panic!("a submit lifts") };
    assert_eq!(w.display_quantity, Some(0.1));
    assert_eq!(
        w.release,
        Some(OrderReleaseWire {
            at: 4_097_217_600_000,
            price: Some(OrderReleasePriceWire::Below(58_000.0)),
            symbol: None,
            venue: None,
        })
    );
    request.release = Some(OrderRelease { at: 4_097_217_600_000, price: Some(on("ETHUSDT")) });
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(
        request.clone(),
    )));
    let Some(CommandWire::Submit(w)) = wire_from_command(&cmd) else { panic!("a submit lifts") };
    assert_eq!(
        w.release.and_then(|r| r.symbol).as_deref(),
        Some("ETHUSDT"),
        "another symbol's condition names it"
    );
    // A condition read on ANOTHER venue names it too (it used to have no wire form at all).
    let elsewhere = OrderReleasePrice { venue: Some("binance".into()), ..on("BTCUSDT") };
    request.release = Some(OrderRelease { at: 4_097_217_600_000, price: Some(elsewhere) });
    let cmd =
        vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(request)));
    let Some(CommandWire::Submit(w)) = wire_from_command(&cmd) else { panic!("a submit lifts") };
    assert_eq!(w.release.and_then(|r| r.venue).as_deref(), Some("binance"));
}

/// The attributes wave's terms cross the lift field for field (they used to have no wire form, so the order was
/// not sent): a limit's attributes, a trailing order's percent, a stop's adjustment to each target.
#[test]
fn a_submit_carries_its_attributes_its_percent_trail_and_its_adjustment() {
    use vike_model::{OrderAttributes, OrderStopAdjust, OrderStopAdjustTo};
    let lift = |request: vike_model::OrderRequest| {
        let cmd =
            vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(request)));
        match wire_from_command(&cmd) {
            Some(CommandWire::Submit(w)) => w,
            other => panic!("a submit lifts, got {other:?}"),
        }
    };
    let attributes = OrderAttributes {
        all_or_none: true,
        sweep_to_fill: false,
        discretionary_amount: Some(0.5),
    };
    let w = lift(vike_model::OrderRequest { attributes: Some(attributes), ..req("c-1") });
    assert_eq!(w.attributes, Some(attributes));
    let empty = OrderAttributes::default();
    let w = lift(vike_model::OrderRequest { attributes: Some(empty), ..req("c-1") });
    assert_eq!(w.attributes, None, "an empty set is no attribute, never the key");
    let trailing = vike_model::OrderRequest {
        order_type: vike_model::OrderType::TrailingTakeProfit,
        price: None,
        trail_percent: Some(1.5),
        ..req("c-2")
    };
    let w = lift(trailing);
    assert_eq!(
        (w.order_type.as_str(), w.trail, w.trail_percent),
        ("trailing_take_profit", None, Some(1.5))
    );
    for to in [
        OrderStopAdjustTo::Stop { stop: 57_000.0 },
        OrderStopAdjustTo::StopLimit { stop: 57_000.0, limit: 56_900.0 },
        OrderStopAdjustTo::Trailing { stop: 57_000.0, trail: None, trail_percent: Some(2.0) },
    ] {
        let stop = vike_model::OrderRequest {
            side: -1,
            order_type: vike_model::OrderType::Stop,
            price: None,
            trigger_price: Some(55_000.0),
            adjust: Some(OrderStopAdjust { at: 60_000.0, to }),
            ..req("c-3")
        };
        assert_eq!(lift(stop).adjust, Some(OrderStopAdjust { at: 60_000.0, to }));
    }
}

/// A trailing Submit's trail and limit offset cross the lift; a `trail` on any other kind (its backtest-only
/// meaning) does not, so that frame is the one from before.
#[test]
fn a_trailing_submit_carries_its_trail_and_offset_and_no_other_kind_sends_a_trail() {
    let trailing = vike_model::OrderRequest {
        order_type: vike_model::OrderType::TrailingLimit,
        price: None,
        trail: Some(50.0),
        limit_offset: Some(5.0),
        ..req("c-1")
    };
    let cmd =
        vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(trailing)));
    let Some(CommandWire::Submit(w)) = wire_from_command(&cmd) else { panic!("a submit lifts") };
    assert_eq!(
        (w.order_type.as_str(), w.trail, w.limit_offset),
        ("trailing_limit", Some(50.0), Some(5.0))
    );
    let limit = vike_model::OrderRequest { trail: Some(50.0), ..req("c-2") };
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(limit)));
    let Some(CommandWire::Submit(w)) = wire_from_command(&cmd) else { panic!("a submit lifts") };
    assert_eq!(w.trail, None, "a limit's backtest trail stays off the wire");
}

/// A Submit's account crosses the lift in the wire spelling (final review I-1). The test above
/// builds its request with `account: None`, so it passes whether the lift copies the account or
/// drops it; this one does not.
#[test]
fn a_submit_carries_its_account_in_the_wire_spelling() {
    use vike_model::accounts::account_keys::AccountLabel;
    let mut request = req("c-1");
    for (account, want) in [
        (None, None),
        (Some(AccountLabel::Default), Some("DEFAULT")),
        (AccountLabel::parse("SUB").ok(), Some("SUB")),
    ] {
        request.account = account;
        let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Submit(Box::new(
            request.clone(),
        )));
        match wire_from_command(&cmd) {
            Some(CommandWire::Submit(w)) => assert_eq!(w.account.as_deref(), want),
            other => panic!("a submit lifts, got {other:?}"),
        }
    }
}

#[test]
fn cancel_maps() {
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Cancel("c-1".into()));
    assert_eq!(wire_from_command(&cmd), Some(CommandWire::Cancel("c-1".into())));
}

#[test]
fn modify_maps() {
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Modify {
        order_client_id: "c-1".into(),
        new_qty: Some(2.0),
        new_price: None,
        new_trigger_price: Some(95.0),
    });
    assert_eq!(
        wire_from_command(&cmd),
        Some(CommandWire::Modify {
            order_client_id: "c-1".into(),
            new_qty: Some(2.0),
            new_price: None,
            new_trigger_price: Some(95.0),
        })
    );
}

#[test]
fn mass_cancel_maps() {
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::MassCancel {
        venue: Some("binance".into()),
        symbol: None,
        account: None,
    });
    assert_eq!(
        wire_from_command(&cmd),
        Some(CommandWire::MassCancel {
            venue: Some("binance".into()),
            symbol: None,
            account: None
        })
    );
}

#[test]
fn flatten_maps() {
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Flatten {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        account: None,
    });
    assert_eq!(
        wire_from_command(&cmd),
        Some(CommandWire::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: None
        })
    );
}

#[test]
fn market_exit_maps() {
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::MarketExit {
        venue: None,
        account: None,
    });
    assert_eq!(
        wire_from_command(&cmd),
        Some(CommandWire::MarketExit { venue: None, account: None })
    );
}

/// A reducing intent that NAMES an account keeps it across the lift — in the wire's spelling,
/// `DEFAULT` included, exactly as `MountStrategy`'s arm renders it. Lifting it to `None` would
/// WIDEN the verb on the far side: an account-less reduce fans out over every account of the
/// venue, so a thin client that dropped the field would flatten books its operator did not
/// name.
#[test]
fn a_labelled_reducing_intent_lifts_its_account_in_the_wire_spelling() {
    use vike_execution::{Command, OrderIntent};
    use vike_model::accounts::account_keys::AccountLabel;
    let alt = || Some(AccountLabel::Named("ALT".into()));
    assert_eq!(
        wire_from_command(&Command::Order(OrderIntent::MassCancel {
            venue: Some("binance".into()),
            symbol: None,
            account: alt(),
        })),
        Some(CommandWire::MassCancel {
            venue: Some("binance".into()),
            symbol: None,
            account: Some("ALT".into()),
        })
    );
    assert_eq!(
        wire_from_command(&Command::Order(OrderIntent::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: alt(),
        })),
        Some(CommandWire::Flatten {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            account: Some("ALT".into()),
        })
    );
    assert_eq!(
        wire_from_command(&Command::Order(OrderIntent::MarketExit {
            venue: Some("binance".into()),
            account: Some(AccountLabel::Default),
        })),
        Some(CommandWire::MarketExit {
            venue: Some("binance".into()),
            account: Some("DEFAULT".into()),
        }),
        "`DEFAULT` is a NAMED account on the wire, not an absent one"
    );
}

#[test]
fn set_trading_state_maps_all_three_arms() {
    for ts in [TradingState::Active, TradingState::Reducing, TradingState::Halted] {
        let cmd = vike_execution::Command::SetTradingState(ts);
        assert_eq!(wire_from_command(&cmd), Some(CommandWire::SetTradingState(ts)));
    }
}

/// A bracket lifts field for field. This was `bracket_has_no_wire_form`, which asserted `None` for
/// the same command while the wire had no bracket and the shell dropped the TP/SL ticket with a log
/// line. It is inverted rather than deleted, so the history stays attached to the pin.
#[test]
fn a_bracket_lifts_field_for_field() {
    let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Bracket(Box::new(
        vike_model::OrderBracket {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
            order_client_id: None,
        },
    )));
    assert_eq!(
        wire_from_command(&cmd),
        Some(CommandWire::Bracket(OrderBracketWire {
            order_client_id: None,
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 1.0,
            entry_price: Some(100.0),
            stop_loss: 95.0,
            take_profit: 110.0,
            account: None,
        }))
    );
    // …and a MARKET entry lifts as `entry_price: None`, the wire's spelling of "enter at market"
    // (m-b of the Task 3 review): the lift must not invent a price for it.
    let market = vike_execution::Command::Order(vike_execution::OrderIntent::Bracket(Box::new(
        vike_model::OrderBracket {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: -1,
            qty: 2.0,
            entry_price: None,
            stop_loss: 105.0,
            take_profit: 90.0,
            account: None,
            order_client_id: None,
        },
    )));
    let Some(CommandWire::Bracket(lifted)) = wire_from_command(&market) else {
        panic!("a market-entry bracket lifts too");
    };
    assert_eq!((lifted.entry_price, lifted.side), (None, -1));
}

/// A bracket's ACCOUNT rides across the lift in the wire spelling (`Display`), the labelled book by its
/// label and the unlabelled one as `DEFAULT`, exactly as a submit's does; naming none lifts to `None`.
/// Dropping it here would put a lane twin's bracket on a node's default book.
#[test]
fn a_brackets_account_lifts_in_the_wire_spelling() {
    use vike_model::accounts::account_keys::AccountLabel;
    let lift = |account: Option<AccountLabel>| {
        let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Bracket(Box::new(
            vike_model::OrderBracket {
                venue: "binance".into(),
                symbol: "BTCUSDT.P".into(),
                side: 1,
                qty: 1.0,
                entry_price: None,
                stop_loss: 95.0,
                take_profit: 110.0,
                account,
                order_client_id: None,
            },
        )));
        match wire_from_command(&cmd) {
            Some(CommandWire::Bracket(b)) => b.account,
            other => panic!("a bracket lifts: {other:?}"),
        }
    };
    assert_eq!(lift(AccountLabel::parse("PERP").ok()).as_deref(), Some("PERP"));
    assert_eq!(lift(Some(AccountLabel::Default)).as_deref(), Some("DEFAULT"));
    assert_eq!(lift(None), None);
}

/// The Trade window's TP/SL toggle reads this, and it is DERIVED from the lift, so it answers true
/// the moment the lift above does. Ruling R8 of the Trade window plan. Asked twice because the
/// answer is cached (`OnceLock`): the second read is the cached one, and must agree.
#[test]
fn a_bracket_has_a_wire_form_so_the_ticket_can_offer_take_profit_stop_loss() {
    assert!(bracket_has_wire_form());
    assert!(bracket_has_wire_form(), "the cached answer is the lift's answer");
}

/// `bracket_has_wire_form` is DERIVED from the lift, never a constant: whatever the lift answers
/// for a bracket, it answers too. Deliberately pins no VALUE — the lift's own bracket pin above is
/// the one pin, so this holds on both sides of the node-side bracket command (Ruling R8, I6).
#[test]
fn bracket_has_wire_form_answers_what_the_lift_answers() {
    for entry_price in [None, Some(100.0)] {
        let cmd = vike_execution::Command::Order(vike_execution::OrderIntent::Bracket(Box::new(
            vike_model::OrderBracket {
                venue: "okx".into(),
                symbol: "ETH-USDT".into(),
                side: -1,
                qty: 3.0,
                entry_price,
                stop_loss: 110.0,
                take_profit: 90.0,
                account: None,
                order_client_id: None,
            },
        )));
        assert_eq!(bracket_has_wire_form(), wire_from_command(&cmd).is_some(), "{cmd:?}");
    }
}

/// **The Trade ticket's Post only asks the node's own gate about the order it would send.** The sample
/// is a LIMIT `Submit` that carries the post-only lifetime key (and no expiry), so a node that does
/// not take that key answers `false` and one that does answers `true`; and a lift that dropped the
/// lifetime (a sample the old node would take) cannot make the control read `true` against it. The
/// POSITIVE half records what the gate was shown, so a stub that answers `true` to anything is
/// caught by the other two.
#[test]
fn a_sample_post_only_order_carries_its_lifetime_key() {
    let mut seen = Vec::new();
    assert!(node_takes_post_only(|wire| {
        seen.push(wire.clone());
        true
    }));
    assert_matches!(
        seen.as_slice(),
        [CommandWire::Submit(OrderRequestWire {
            order_type,
            price: Some(_),
            time_in_force: Some(word),
            gtd_expiry: None,
            ..
        })] if order_type == "limit" && word == "post-only"
    );
    assert!(!node_takes_post_only(|_| false), "a node that takes nothing");
    // The node from before the word takes a submit that names no lifetime, and only that.
    let old =
        |wire: &CommandWire| matches!(wire, CommandWire::Submit(r) if r.time_in_force.is_none());
    assert!(!node_takes_post_only(old), "the old node would drop the key: the control is off");
}

/// **The planner's bracket stem asks the node's own gate about a bracket that carries one**: a node
/// without `bracket-stem` takes a stem-less bracket only, so the stem is off and the bracket goes as
/// before, never refused whole.
#[test]
fn a_sample_stem_bracket_carries_its_stem() {
    let mut seen = Vec::new();
    assert!(node_takes_bracket_stem(|wire| {
        seen.push(wire.clone());
        true
    }));
    assert_matches!(
        seen.as_slice(),
        [CommandWire::Bracket(crate::wire::OrderBracketWire { order_client_id: Some(_), .. })]
    );
    let old =
        |wire: &CommandWire| matches!(wire, CommandWire::Bracket(b) if b.order_client_id.is_none());
    assert!(!node_takes_bracket_stem(old), "the old node refuses the key: no stem");
}

/// An ORDER LIST lifts field for field: a ladder's legs, links and kinds in the wire's words, a trailing leg's trail, a
/// lifetime other than `gtc` by its word (and `gtc` as the absent field), the account in the wire spelling.
#[test]
fn an_order_list_lifts_field_for_field() {
    use vike_model::ContingencyType;
    let mut spec = vike_model::build_ladder(&vike_model::LadderSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT.P".into(),
        side: 1,
        qty: 3.0,
        entry_price: Some(100.0),
        stop_loss: 90.0,
        targets: vec![vike_model::LadderTarget { price: 110.0, qty: 3.0 }],
        account: vike_model::accounts::account_keys::parse_wire_account("PERP").ok(),
    })
    .expect("a valid ladder");
    spec.legs[0].time_in_force = vike_model::TimeInForce::Day;
    spec.legs[1].order_type = vike_model::OrderType::Trailing;
    spec.legs[1].trail = Some(5.0);
    for (i, leg) in spec.legs.iter_mut().enumerate() {
        leg.order_client_id = Some(format!("g{i}"));
    }
    let cmd =
        vike_execution::Command::Order(vike_execution::OrderIntent::OrderList(Box::new(spec)));
    let Some(CommandWire::OrderList(l)) = wire_from_command(&cmd) else {
        panic!("an order list lifts")
    };
    assert_eq!((l.venue.as_str(), l.symbol.as_str()), ("binance", "BTCUSDT.P"));
    assert_eq!(l.account.as_deref(), Some("PERP"));
    assert_eq!(l.legs.len(), 3);
    assert_eq!(
        (l.legs[0].order_type.as_str(), l.legs[0].price, l.legs[0].time_in_force.as_deref()),
        ("limit", Some(100.0), Some("day"))
    );
    assert_eq!(
        (l.legs[1].order_type.as_str(), l.legs[1].trigger_price, l.legs[1].parent),
        ("trailing", Some(90.0), Some(0))
    );
    assert_eq!(
        (l.legs[0].trail, l.legs[1].trail),
        (None, Some(5.0)),
        "a trailing leg's trail rides"
    );
    assert_eq!(l.legs[1].contingency, Some(ContingencyType::Ouo));
    assert_eq!(l.legs[1].linked, vec![2]);
    assert!(l.legs[2].reduce_only && l.legs[2].time_in_force.is_none(), "gtc is the absent field");
    let ids: Vec<_> = l.legs.iter().map(|leg| leg.order_client_id.as_deref()).collect();
    assert_eq!(ids, [Some("g0"), Some("g1"), Some("g2")], "each leg's own id rides across");
}

/// A COMBO lifts field for field: legs, signed ratios, leg limits, its one id and the account in the wire spelling,
/// and every lifetime by its word (`gtc`, the default, as the absent field), never dropped.
#[test]
fn a_combo_lifts_field_for_field_and_never_drops_a_lifetime() {
    let leg = |symbol: &str, ratio, limit| vike_model::OrderComboLeg {
        symbol: symbol.into(),
        ratio,
        limit,
    };
    let mut spec = vike_model::OrderCombo {
        venue: "deribit".into(),
        side: -1,
        qty: 2.0,
        legs: vec![leg("C1", 1, Some(0.05)), leg("C2", -2, Some(0.02))],
        net_limit: None,
        time_in_force: vike_model::TimeInForce::default(),
        account: vike_model::accounts::account_keys::parse_wire_account("DEFAULT").ok(),
        order_client_id: Some("g1combo".into()),
    };
    let lift = |spec: &vike_model::OrderCombo| {
        wire_from_command(&vike_execution::Command::Order(vike_execution::OrderIntent::Combo(
            Box::new(spec.clone()),
        )))
    };
    let Some(CommandWire::Combo(c)) = lift(&spec) else { panic!("a combo lifts") };
    assert_eq!((c.venue.as_str(), c.side, c.qty, c.net_limit), ("deribit", -1, 2.0, None));
    assert_eq!(c.account.as_deref(), Some("DEFAULT"), "the unlabelled book, named");
    let legs: Vec<_> = c.legs.iter().map(|l| (l.symbol.as_str(), l.ratio, l.limit)).collect();
    assert_eq!(legs, [("C1", 1, Some(0.05)), ("C2", -2, Some(0.02))]);
    assert_eq!(c.order_client_id.as_deref(), Some("g1combo"), "its one id rides across");
    assert_eq!(c.time_in_force, None, "gtc is the absent field");
    for tif in
        [vike_model::TimeInForce::Ioc, vike_model::TimeInForce::Fok, vike_model::TimeInForce::Gtd]
    {
        spec.time_in_force = tif;
        let Some(CommandWire::Combo(c)) = lift(&spec) else { panic!("{tif:?} lifts") };
        assert_eq!(c.time_in_force.as_deref(), Some(tif.as_word()), "never dropped");
    }
}

#[test]
fn set_margin_and_shutdown_have_no_wire_form() {
    let margin = vike_execution::Command::SetMargin(Box::new(vike_execution::MarginUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        initial_margin_requirement: 0.1,
    }));
    assert_eq!(wire_from_command(&margin), None);
    assert_eq!(wire_from_command(&vike_execution::Command::Shutdown), None);
}

/// The operator's reconcile confirm lifts onto the wire verb the daemon lowers back to
/// `Command::ConfirmReconcile`.
#[test]
fn confirm_reconcile_maps_onto_the_reconcile_confirm_wire_verb() {
    assert_eq!(
        wire_from_command(&vike_execution::Command::ConfirmReconcile(9)),
        Some(crate::wire::CommandWire::ReconcileConfirm { id: 9 })
    );
}

/// The B4 strategy write verb maps: the target key copies verbatim and the typed
/// `StrategyParams` re-serializes into the core's OWN serde JSON — byte-for-byte the value
/// `serde_json::to_value` produces from the core type, which is what the daemon's
/// `lower_command` deserializes back (the delegated-schema contract).
#[test]
fn update_params_maps_onto_the_delegated_core_json() {
    let params = vike_model::StrategyParams::SpreadMaker(vike_model::SpreadMakerParams {
        qty: 2.0,
        half_spread: 1.0,
        target_inventory: 0.0,
        max_inventory: 1.0,
        skew: 0.0,
        fill_window_ms: 0,
        net_fill_threshold: 0.0,
        suppress_cooldown_ms: 0,
        style: vike_model::QuoteStyle::Mid,
        depth_levels: 1,
        tick_size: 0.0,
        filter_own: false,
        avellaneda_stoikov: None,
        refresh_tolerance: None,
        ladder: None,
        reward: None,
        toxicity: None,
    });
    let cmd = vike_execution::Command::UpdateParams(Box::new(vike_execution::ParamsUpdate {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        mount_id: Some("maker-a".into()),
        params,
    }));
    let Some(CommandWire::UpdateParams { venue, symbol, interval, mount_id, params: wire_params }) =
        wire_from_command(&cmd)
    else {
        panic!("UpdateParams has a wire form since split-plane B4");
    };
    assert_eq!((venue.as_str(), symbol.as_str(), interval.as_str()), ("binance", "BTCUSDT", "1m"));
    assert_eq!(mount_id.as_deref(), Some("maker-a"), "the mount id crosses the lift verbatim");
    let vike_execution::Command::UpdateParams(u) = &cmd else { unreachable!() };
    assert_eq!(
        wire_params,
        serde_json::to_value(&u.params).unwrap(),
        "the wire payload IS the core type's own serde JSON, not a lookalike"
    );
}

/// A runtime mount's `params` table lifts onto the wire as the JSON object it was read from,
/// byte for byte — the integer an integer, `2.0` a float, the nested table and the array intact.
#[test]
fn a_mount_lifts_its_params_table_into_the_same_json() {
    const GOLDEN: &str = r#"{"levels":[1,2.5,"a"],"nested":{"flag":true},"rungs":2,"size":2.0}"#;
    let cmd = vike_execution::Command::MountStrategy(Box::new(vike_execution::MountSpec {
        venue: "binance".into(),
        symbol: "BTCUSDT".into(),
        interval: "1m".into(),
        account: None,
        controller_id: Some("rt-a".into()),
        file: None,
        name: Some("grid".into()),
        rhai: None,
        params: serde_json::from_str(GOLDEN).unwrap(),
        order_client_id_code: None,
    }));
    let Some(CommandWire::MountStrategy { params, name, controller_id, .. }) =
        wire_from_command(&cmd)
    else {
        panic!("MountStrategy has a wire form since split-plane B5");
    };
    assert_eq!((name.as_deref(), controller_id.as_deref()), (Some("grid"), Some("rt-a")));
    assert_eq!(serde_json::to_string(&params).unwrap(), GOLDEN);
}
