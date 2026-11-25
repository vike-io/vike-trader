//! The ticket's orders: Buy at the typed price, a live account's confirm, Post only, a quote
//! amount, a greyed account row.

use super::*;

/// The Buy button sends a limit at the typed price (one-click is on for DEMO).
#[test]
fn the_ticket_buy_sends_a_limit_at_the_typed_price() {
    let mut h = settled(demo());
    set(&mut h, |s| s.price = "99.5".to_string());
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    match places(h.state()).as_slice() {
        [
            TradeWindowAction::Place {
                side: 1,
                order_type: TradeWindowTicketKind::Limit,
                price: Some(p),
                quantity,
                reduce_only: false,
                post_only: false,
                exits: None,
                origin: TradeWindowOrigin::Ticket,
            },
        ] => {
            assert!((p - 99.5).abs() < 1e-9 && (quantity - 0.01).abs() < 1e-12, "{p} {quantity}");
        }
        other => panic!("one limit buy, got {other:?}"),
    }
}

/// One-click starts on for DEMO and off for LIVE; on LIVE a click is HELD for a confirm, and the
/// confirm sends exactly the order that was held.
#[test]
fn a_live_account_holds_every_order_for_a_confirm() {
    let mut h = settled(live());
    assert!(!h.state().state.one_click, "LIVE starts with one-click off");
    set(&mut h, |s| s.price = "99.5".to_string());
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    assert!(places(h.state()).is_empty(), "nothing is sent before the confirm");
    let held = h.state().state.held.clone().expect("the order is held");
    click(&mut h, "Place");
    assert_eq!(places(h.state()), [&held], "the confirm sends the held order");
}

/// Changing the instrument drops a held order, so a confirm can never send it to the new one
/// (Review Focus 4).
#[test]
fn an_instrument_change_drops_the_held_order() {
    let mut h = settled(live());
    h.state_mut().state.held = Some(TradeWindowAction::CancelAll);
    h.state_mut().scene.account = Some("SUB");
    h.run();
    assert!(h.state().state.held.is_none());
}

/// A symbol the account does not trade produces no order, and the ticket says why (Review Focus 2,
/// pre-flight I8's ticket half). POSITIVE CONTROL first: the same click on the same window, where
/// the account does trade the symbol, places the order, so the untradable half cannot pass by
/// clicking nothing.
#[test]
fn an_untradable_symbol_produces_no_order_and_says_why() {
    let reason = "This account does not trade BTCUSDT. It trades ETHUSDT.";
    for tradable in [true, false] {
        let mut h = settled(Scene { tradable, ..demo() });
        set(&mut h, |s| s.price = "99.5".to_string());
        for side in ["Buy 0.010 limit", "Sell 0.010 limit"] {
            let button = h.get_by_label(side);
            assert_eq!(button.accesskit_node().is_disabled(), !tradable, "{side}, {tradable}");
            button.click();
            h.run();
        }
        if tradable {
            assert_eq!(places(h.state()).len(), 2, "the positive control: {:?}", h.state().emitted);
        } else {
            assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
            assert!(h.state().state.held.is_none(), "nothing is held either");
            assert!(h.query_all_by_label(reason).next().is_some(), "the reason is on screen");
            assert!(says_on_hover(&mut h, "Buy 0.010 limit", reason), "and on the Buy button");
        }
    }
}

/// Leverage is drawn, disabled, with the owner's words on hover, and a click on it produces no
/// intent at all (spec §§3.7 and 7, Ruling R2, pre-flight Minor 13). Post only used to be drawn the
/// same way, with the owner's words that Vike cannot send a post-only order; it can now, and has
/// its own tests below.
#[test]
fn leverage_is_drawn_disabled_says_why_and_sends_nothing() {
    let mut h = settled(demo());
    let (name, why) = ("Leverage", ticket::LEVERAGE_WHY);
    assert!(disabled(&h, name), "{name} is disabled");
    assert!(says_on_hover(&mut h, name, why), "{name}: the owner's words show on hover");
    click(&mut h, name);
    assert!(h.state().emitted.is_empty(), "{name}: {:?}", h.state().emitted);
    assert!(h.state().state.held.is_none(), "{name}: nothing is held");
}

/// **Post only is a real control where the venue has a post-only order and the node carries it**:
/// enabled, a toggle that reads pressed to a screen reader, and the order it sends is a post-only
/// LIMIT order. Its positive control is the same window with the toggle off, which sends none; and
/// a click on the toggle itself sends nothing by itself.
#[test]
fn post_only_is_a_live_toggle_that_makes_the_order_post_only() {
    let mut h = settled(maker());
    set(&mut h, |s| s.price = "99.5".to_string());
    let node = |h: &Harness<'_, Fixture>| {
        let n = h.get_by_label("Post only").accesskit_node();
        (n.role(), n.toggled(), n.is_disabled())
    };
    assert_eq!(node(&h), (Role::Button, Some(Toggled::False), false), "enabled, not pressed");
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    assert_matches!(
        places(h.state()).as_slice(),
        [TradeWindowAction::Place {
            order_type: TradeWindowTicketKind::Limit,
            post_only: false,
            ..
        }]
    );
    h.state_mut().emitted.clear();

    click(&mut h, "Post only");
    assert!(h.state().state.post_only, "the click pressed it");
    assert_eq!(node(&h), (Role::Button, Some(Toggled::True), false));
    assert!(h.state().emitted.is_empty(), "the toggle sends nothing by itself");
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    match places(h.state()).as_slice() {
        [
            TradeWindowAction::Place {
                side: 1,
                order_type: TradeWindowTicketKind::Limit,
                price: Some(p),
                reduce_only: false,
                post_only: true,
                exits: None,
                origin: TradeWindowOrigin::Ticket,
                ..
            },
        ] => assert!((p - 99.5).abs() < 1e-9, "{p}"),
        other => panic!("one post-only limit buy, got {other:?}"),
    }
    h.state_mut().emitted.clear();
    click(&mut h, "Post only");
    assert!(!h.state().state.post_only, "a second click lets go of it");
    assert_eq!(node(&h), (Role::Button, Some(Toggled::False), false));
}

/// **A greyed Post only says the TRUE reason**, one of three, and sends nothing: the venue has no
/// post-only order (the cause no update cures, so it outranks the others), the node is too old to
/// carry one (an older node would drop the flag and run a plain limit order that takes liquidity),
/// or the ticket is not on a Limit order. The positive control is [`post_only_is_a_live_toggle_that_makes_the_order_post_only`];
/// here every cause is drawn, hovered and clicked, and the state is never ticked by it.
#[test]
fn a_greyed_post_only_says_the_true_reason_and_sends_nothing() {
    let no_venue = Scene { capabilities: VenueCapabilities::UNSUPPORTED, ..maker() };
    let old_node = Scene { post_only_wire: false, ..maker() };
    let both = Scene { post_only_wire: false, ..no_venue.clone() };
    let venue_why = ticket::post_venue_why("Binance");
    let venue_why = venue_why.as_str();
    let (market, stop) = (Some(TradeWindowTicketKind::Market), Some(TradeWindowTicketKind::Stop));
    let cases: [(&str, Scene, Option<TradeWindowTicketKind>, &str); 6] = [
        ("the venue has none", no_venue, None, venue_why),
        ("the node is too old", old_node.clone(), None, ticket::POST_NODE_WHY),
        ("a market order", maker(), market, ticket::POST_LIMIT_WHY),
        ("a stop order", maker(), stop, ticket::POST_LIMIT_WHY),
        ("no venue AND an old node: the venue's, no update cures it", both, None, venue_why),
        ("an old node on a market order: the node's", old_node, market, ticket::POST_NODE_WHY),
    ];
    for (what, scene, kind, why) in cases {
        let mut h = settled(scene);
        if let Some(kind) = kind {
            set(&mut h, |s| s.order_type = kind);
        }
        assert!(disabled(&h, "Post only"), "{what}: disabled");
        assert!(says_on_hover(&mut h, "Post only", why), "{what}: the reason shows on hover");
        click(&mut h, "Post only");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(!h.state().state.post_only, "{what}: a greyed click ticks nothing");
    }
}

/// **Post only never stays ticked where the order could not keep it.** Ticked on a Limit, it is turned
/// off in view when the trader picks Market or Stop, when the node turns out too old (a reconnect to
/// an older one) and when the venue has none, and a Buy sent after is a plain order, never a taker
/// the trader thought was a maker. Back on Limit it is available again, but not pressed.
#[test]
fn post_only_is_turned_off_when_the_order_can_no_longer_keep_it() {
    let mut h = settled(maker());
    set(&mut h, |s| {
        s.price = "99.5".to_string();
        s.post_only = true;
    });
    set(&mut h, |s| s.order_type = TradeWindowTicketKind::Market);
    assert!(!h.state().state.post_only, "a market order drops it");
    assert!(disabled(&h, "Post only"), "and the button says it is not available");
    set(&mut h, |s| s.order_type = TradeWindowTicketKind::Limit);
    assert!(!h.state().state.post_only, "back on Limit it is not pressed again by itself");
    assert!(!disabled(&h, "Post only"), "...but it is available");

    type Change = fn(&mut Scene);
    let changes: [(&str, Change); 2] = [
        ("an old node", |s| s.post_only_wire = false),
        ("a venue with none", |s| s.capabilities = VenueCapabilities::UNSUPPORTED),
    ];
    for (what, change) in changes {
        set(&mut h, |s| s.post_only = true);
        assert!(h.state().state.post_only, "CONTROL: ticked before {what}");
        change(&mut h.state_mut().scene);
        h.run();
        assert!(!h.state().state.post_only, "{what} turns it off");
        h.state_mut().emitted.clear();
        h.get_by_label_contains("Buy 0.010 limit").click();
        h.run();
        assert_matches!(
            places(h.state()).as_slice(),
            [TradeWindowAction::Place { post_only: false, .. }],
            "{what}: the order that goes out is the plain one the window shows"
        );
        h.state_mut().emitted.clear();
        h.state_mut().scene = maker();
        h.run();
    }
}

/// The money path, called directly: an amount typed in the quote currency becomes the CONTRACT's
/// quantity (the wave-2 audit's ticket defect). 250,000 on an ES future (50 a point) at 5,000 is ONE
/// contract, where `value / price` sent 50; 1,000 on Deribit's `BTC-PERPETUAL` (a dollar a contract)
/// is 1,000 contracts, where `value / price` made it 0.0153, under one lot, and sent nothing. The
/// default unit sends what it always did.
#[test]
fn a_quote_amount_is_ordered_in_the_windows_contract_units() {
    let book = book();
    let es_grid =
        Grid { tick_size: Some(0.25), lot_size: Some(1.0), min_notional: None, min_qty: Some(1.0) };
    let order = |value, price: &str, size: &str| {
        let state = state_with(|s| {
            s.price = price.to_string();
            s.size = size.to_string();
            s.unit = TradeWindowSizingUnit::Quote;
        });
        let inputs = TradeWindowInputs { value, ..inputs_on(&book, es_grid) };
        match ticket::order(1, &state, &inputs) {
            Ok(TradeWindowAction::Place { quantity, .. }) => quantity,
            other => panic!("{other:?}"),
        }
    };
    use vike_model::ContractValue;
    assert_eq!(order(ContractValue::linear(50.0), "5000", "250000"), 1.0);
    assert_eq!(order(ContractValue::inverse(1.0), "65432.5", "1000"), 1_000.0);
    assert_eq!(order(ContractValue::default(), "5000", "250000"), 50.0, "the default unit");
}

/// The money path, called directly: the ticket's order carries the post-only flag exactly where the
/// toggle is ticked on a Limit order, and a ticked toggle where it cannot be kept is REFUSED with
/// the reason, never sent as a taker (the net under [`TradeWindowState::sync`]'s turning it off); and with
/// TP/SL on as well it is refused, because a bracket's entry carries no lifetime.
#[test]
fn the_ticket_order_carries_post_only_or_is_refused() {
    let book = book();
    let post = |inputs: &TradeWindowInputs<'_>, kind, take_profit_stop_loss| {
        let state = state_with(|s| {
            s.order_type = kind;
            s.price = "99.5".to_string();
            s.size = "0.010".to_string();
            s.post_only = true;
            s.take_profit_stop_loss = take_profit_stop_loss;
        });
        ticket::order(1, &state, inputs)
    };
    let capable = TradeWindowInputs {
        capabilities: VenueCapabilities {
            supports_post_only: true,
            ..VenueCapabilities::UNSUPPORTED
        },
        post_only_wire: true,
        ..inputs_on(&book, BTC)
    };
    assert_matches!(
        post(&capable, TradeWindowTicketKind::Limit, false),
        Ok(TradeWindowAction::Place {
            order_type: TradeWindowTicketKind::Limit,
            post_only: true,
            exits: None,
            ..
        })
    );
    assert_eq!(
        post(&capable, TradeWindowTicketKind::Market, false),
        Err(ticket::POST_LIMIT_WHY.to_string())
    );
    assert_eq!(
        post(&capable, TradeWindowTicketKind::Stop, false),
        Err(ticket::POST_LIMIT_WHY.to_string())
    );
    let old = TradeWindowInputs { post_only_wire: false, ..capable };
    assert_eq!(
        post(&old, TradeWindowTicketKind::Limit, false),
        Err(ticket::POST_NODE_WHY.to_string())
    );
    let none = TradeWindowInputs { capabilities: VenueCapabilities::UNSUPPORTED, ..capable };
    assert_eq!(
        post(&none, TradeWindowTicketKind::Limit, false),
        Err(ticket::post_venue_why("Binance")),
        "a venue with none refuses it"
    );
    assert_eq!(
        post(&capable, TradeWindowTicketKind::Limit, true),
        Err(ticket::POST_TAKE_PROFIT_STOP_LOSS_WHY.to_string())
    );
    // CONTROL: the same ticket without the toggle is the plain order, with or without TP/SL.
    let plain = |take_profit_stop_loss| {
        let state = state_with(|s| {
            s.price = "99.5".to_string();
            s.size = "0.010".to_string();
            s.take_profit_stop_loss = take_profit_stop_loss;
        });
        ticket::order(1, &state, &capable)
    };
    assert_matches!(plain(false), Ok(TradeWindowAction::Place { post_only: false, .. }));
    assert_matches!(
        plain(true),
        Ok(TradeWindowAction::Place { post_only: false, exits: Some(_), .. })
    );
}

/// Post only and TP/SL ticked together: Buy and Sell are disabled with the reason on hover and the
/// strip says it, and neither toggle is turned off for the trader. Turning one off sends the order.
#[test]
fn post_only_with_take_profit_stop_loss_refuses_the_order_in_words_and_keeps_both_ticked() {
    let mut h = settled(maker());
    set(&mut h, |s| {
        s.price = "99.5".to_string();
        s.post_only = true;
        s.take_profit_stop_loss = true;
    });
    assert!(h.state().state.post_only && h.state().state.take_profit_stop_loss, "both stay ticked");
    assert!(disabled(&h, "Buy 0.010 limit"), "the order is refused");
    assert!(
        in_strip(&h).iter().any(|p| p.text == ticket::POST_TAKE_PROFIT_STOP_LOSS_WHY),
        "the strip says why it will not send"
    );
    assert!(
        h.query_all_by_label(ticket::POST_TAKE_PROFIT_STOP_LOSS_WHY).count() >= 1,
        "and the form writes it under the toggles, where the exit previews would be"
    );
    assert_eq!(
        h.query_all_by_label_contains("Buy: ").count(),
        0,
        "in place of exit previews that would repeat it"
    );
    assert!(
        says_on_hover(&mut h, "Buy 0.010 limit", ticket::POST_TAKE_PROFIT_STOP_LOSS_WHY),
        "and the Buy says it"
    );
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    assert!(places(h.state()).is_empty(), "nothing is sent: {:?}", h.state().emitted);
    set(&mut h, |s| s.take_profit_stop_loss = false);
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    assert_matches!(
        places(h.state()).as_slice(),
        [TradeWindowAction::Place { post_only: true, .. }]
    );
}

/// The compact ticket sends market orders only, so its `Post` is greyed with the reason that is true
/// for it: a venue or a node that cannot is said first, else that post-only needs a Limit order.
#[test]
fn the_compact_tickets_post_is_greyed_with_the_true_reason() {
    for (scene, why) in [
        (maker(), ticket::POST_COMPACT_WHY.to_string()),
        (Scene { post_only_wire: false, ..maker() }, ticket::POST_NODE_WHY.to_string()),
        (
            Scene { capabilities: VenueCapabilities::UNSUPPORTED, ..maker() },
            ticket::post_venue_why("Binance"),
        ),
    ] {
        let mut h = harness_at(scene, trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
        h.run();
        h.state_mut().emitted.clear();
        assert!(disabled(&h, "Post"), "greyed in the compact ticket");
        assert!(says_on_hover(&mut h, "Post", &why), "{why}");
        click(&mut h, "Post");
        assert!(h.state().emitted.is_empty() && !h.state().state.post_only);
    }
}

/// An account the settings database holds but the server does not run is listed greyed, says why
/// on hover, and takes no click. This is spec §3.3 and the owner's ruling of 2026-09-30 that the
/// list comes from the database.
#[test]
fn a_greyed_account_row_says_why_and_takes_no_click() {
    let row = |account, name, why_not| TradeWindowAccountPickerRow {
        venue: "binance",
        venue_label: "Binance",
        product: "",
        account,
        symbol: "BTCUSDT",
        name,
        mode: Some(AccountTier::Demo),
        why_not,
        lost: None,
    };
    let accounts = vec![
        row(None, "main", None),
        row(Some("HEDGE"), "HEDGE", Some("Not running on the server.")),
    ];
    let mut h = settled(Scene { accounts, ..demo() });
    h.get_by_label_contains("Binance · main").click();
    h.run();
    assert!(
        says_on_hover(&mut h, "Binance, HEDGE · BTCUSDT, DEMO", "Not running on the server."),
        "the reason shows on hover"
    );
    click(&mut h, "Binance, HEDGE · BTCUSDT, DEMO");
    assert!(
        !h.state().emitted.iter().any(|a| matches!(a, TradeWindowAction::PickAccount { .. })),
        "a greyed row picks nothing: {:?}",
        h.state().emitted
    );
}
