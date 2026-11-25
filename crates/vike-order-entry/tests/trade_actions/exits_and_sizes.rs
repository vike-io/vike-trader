//! TP/SL where it is offered and refused, the confirm's exits, the tick and the size rules, and an
//! order that never drops an exit.

use super::*;

/// TP/SL is offered on a venue's single default book for a Market or Limit entry (the POSITIVE
/// CONTROL, pre-flight Minor 12: a toggle that is always disabled fails here). Everywhere else,
/// while it is OFF, it cannot be turned on: it is disabled and says why.
#[test]
fn take_profit_stop_loss_is_offered_only_on_a_single_default_book_for_a_market_or_limit_entry() {
    for order_type in [TradeWindowTicketKind::Limit, TradeWindowTicketKind::Market] {
        let mut h = settled(demo());
        set(&mut h, |s| s.order_type = order_type);
        assert!(!disabled(&h, "TP/SL"), "{order_type:?}: TP/SL is offered");
    }
    for (what, scene, tweak, why) in refused_take_profit_stop_loss() {
        let mut h = settled(scene);
        set(&mut h, tweak);
        let toggle = h.get_by_label("TP/SL").accesskit_node();
        assert!(toggle.is_disabled(), "{what}: TP/SL cannot be turned on");
        assert_eq!(toggle.toggled(), Some(Toggled::False), "{what}");
        assert!(says_on_hover(&mut h, "TP/SL", why), "{what}: {why}");
    }
}

/// Fix round 1, I-3 (RULED): TP/SL TICKED where it is refused refuses the ORDER, never sends it
/// without its exits. The toggle stays ticked (it is the trader's setting), is enabled only to be
/// turned off, and the reason is written under it, not only on hover — and, since FW6 (I3), the
/// status strip says it too, where it said "Ready". Buy and Sell send nothing and hold nothing;
/// once TP/SL is off the same order goes, plain.
#[test]
fn a_ticked_take_profit_stop_loss_that_is_refused_refuses_the_order_until_it_is_turned_off() {
    for (what, scene, tweak, why) in refused_take_profit_stop_loss() {
        let mut h = settled(scene);
        set(&mut h, |s| {
            tweak(s);
            s.price = "100".to_string();
            s.take_profit_stop_loss = true;
        });
        let refusal = ticket::take_profit_stop_loss_refusal(why);
        assert!(refusal.contains("Turn TP/SL off"), "{refusal}");
        let toggle = h.get_by_label("TP/SL").accesskit_node();
        assert!(!toggle.is_disabled(), "{what}: it can be turned off");
        assert_eq!(toggle.toggled(), Some(Toggled::True), "{what}: the trader's setting shows");
        let written = |region| labels_in(&h, region).contains(&refusal);
        assert!(written("Order ticket"), "{what}: the reason is written, not hovered");
        assert!(written("Status strip"), "{what}: and the strip says it, not Ready");
        let kind = if h.state().state.order_type == TradeWindowTicketKind::Stop {
            "stop"
        } else {
            "limit"
        };
        let (buy, sell) = (format!("Buy 0.010 {kind}"), format!("Sell 0.010 {kind}"));
        for button in [&buy, &sell] {
            assert!(disabled(&h, button), "{what}: {button}");
            click(&mut h, button);
        }
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: nothing is held either");
        assert!(says_on_hover(&mut h, &buy, &refusal), "{what}: on the button");
        click(&mut h, "TP/SL");
        assert!(!h.state().state.take_profit_stop_loss, "{what}: the click turned it off");
        assert!(disabled(&h, "TP/SL"), "{what}: and it cannot be turned back on here");
        click(&mut h, &buy);
        match places(h.state()).as_slice() {
            [TradeWindowAction::Place { side: 1, exits: None, .. }] => {}
            other => panic!("{what}: one plain order once TP/SL is off, got {other:?}"),
        }
    }
}

/// Fix round 1, I-3 case (a): a trader with TP/SL set moves the window to a named account. The
/// setting survives the move, and the order is then REFUSED rather than sent without its exits.
#[test]
fn a_move_to_a_named_account_keeps_take_profit_stop_loss_and_refuses_the_order() {
    let mut h = settled(live());
    set(&mut h, |s| s.take_profit_stop_loss = true);
    h.state_mut().scene.account = Some("SUB");
    h.state_mut().scene.bracket_why = Some(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY);
    h.run();
    set(&mut h, |s| s.price = "100".to_string());
    assert!(h.state().state.take_profit_stop_loss, "the trader's setting survives the move");
    assert!(disabled(&h, "Buy 0.010 limit"));
    click(&mut h, "Buy 0.010 limit");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    assert!(h.state().state.held.is_none(), "nothing is held for a confirm either");
    let refusal = ticket::take_profit_stop_loss_refusal(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY);
    assert!(says_on_hover(&mut h, "Buy 0.010 limit", &refusal));
}

/// Fix round 1, I-3: the confirm prompt says what the held order carries: its exits, or that it
/// has none.
#[test]
fn the_confirm_prompt_names_the_exits_or_says_there_are_none() {
    for (take_profit_stop_loss, words) in [(true, "TP 100.5 / SL 99.7"), (false, "no TP/SL")] {
        let mut h = settled(live());
        set(&mut h, |s| {
            s.price = "100".to_string();
            s.take_profit_stop_loss = take_profit_stop_loss;
        });
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().state.held.is_some(), "{take_profit_stop_loss}: held");
        assert!(
            h.query_all_by_label_contains(words).next().is_some(),
            "{take_profit_stop_loss}: {words}"
        );
    }
}

/// Pre-flight Minor 8: the reasons rank wire > account > Stop entry > reduce-only.
#[test]
fn the_take_profit_stop_loss_refusals_rank_node_account_entry_reduce_only() {
    let b = book();
    let all = state_with(|s| s.reduce_only = true);
    let free = state_with(|_| {});
    let i = |wire, why| TradeWindowInputs {
        bracket_wire: wire,
        bracket_why: why,
        ..inputs_on(&b, BTC)
    };
    let (stop, stated) =
        (TradeWindowTicketKind::Stop, Some(ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY));
    assert_eq!(
        ticket::take_profit_stop_loss_block(&all, &i(false, stated), stop),
        Some(ticket::TAKE_PROFIT_STOP_LOSS_WIRE_WHY)
    );
    assert_eq!(ticket::take_profit_stop_loss_block(&all, &i(true, stated), stop), stated);
    assert_eq!(
        ticket::take_profit_stop_loss_block(&all, &i(true, None), stop),
        Some(ticket::TAKE_PROFIT_STOP_LOSS_STOP_WHY)
    );
    let limit = TradeWindowTicketKind::Limit;
    assert_eq!(
        ticket::take_profit_stop_loss_block(&all, &i(true, None), limit),
        Some(ticket::TAKE_PROFIT_STOP_LOSS_REDUCE_WHY)
    );
    assert_eq!(
        ticket::take_profit_stop_loss_block(&free, &i(true, None), limit),
        None,
        "the positive control"
    );
    // The stated cause is said as stated, whatever it is: the glue's lane reason, for one.
    let lane = "TP/SL is not available on this account yet: its engine trades a spot market.";
    assert_eq!(ticket::take_profit_stop_loss_block(&free, &i(true, Some(lane)), limit), Some(lane));
}

/// Fix round 1, I-1: a held order is dropped, and the window says so, the moment the window can
/// no longer trade (the account stops trading the symbol, halts or faults): its Place must never
/// reach a venue past that. POSITIVE CONTROL: the same steps on a window that still trades keep
/// the order held.
#[test]
fn a_held_order_is_dropped_when_the_window_can_no_longer_trade() {
    for still_tradable in [true, false] {
        let mut h = settled(live());
        set(&mut h, |s| s.price = "99.5".to_string());
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().state.held.is_some(), "held for a confirm");
        h.state_mut().emitted.clear();
        h.state_mut().scene.tradable = still_tradable;
        h.run();
        if still_tradable {
            assert!(h.state().state.held.is_some(), "the positive control keeps it");
            assert!(h.query_by_label("Place").is_some());
            assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
        } else {
            assert!(h.state().state.held.is_none(), "dropped");
            assert!(h.query_by_label("Place").is_none(), "nothing left to confirm");
            match h.state().emitted.as_slice() {
                [TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Error, text }] => {
                    assert!(text.starts_with("Not sent:"), "{text}");
                }
                other => panic!("one error note, got {other:?}"),
            }
        }
    }
}

/// Fix round 1, I-2 (RULED): a typed price off the instrument's tick is REFUSED, never sent as
/// typed (the core would round it, past the trader's limit, and its exits would be measured from
/// a price the order does not sit at). A tick multiple is sent, whatever float noise the tick
/// carries; a tick nobody gave is not checked (no lot is known then either, so nothing is sent).
#[test]
fn an_off_tick_price_is_refused_and_a_tick_multiple_is_sent() {
    let b = book();
    let on = |tick| inputs_on(&b, Grid { tick_size: grid_value(tick), ..BTC });
    let limit = |price: &str, take_profit_stop_loss: bool| {
        let price = price.to_string();
        state_with(move |s| {
            s.size = "0.010".to_string();
            s.price = price;
            s.take_profit_stop_loss = take_profit_stop_loss;
        })
    };
    for (price, take_profit_stop_loss) in [("100.4", true), ("100.6", false)] {
        assert_eq!(
            ticket::order(1, &limit(price, take_profit_stop_loss), &on(1.0)),
            Err(format!("{price} is not on the 1 tick: use a multiple of 1.")),
            "{price}"
        );
    }
    let stop = state_with(|s| {
        s.size = "0.010".to_string();
        s.price = "100.6".to_string();
        s.order_type = TradeWindowTicketKind::Stop;
    });
    assert!(ticket::order(1, &stop, &on(1.0)).is_err(), "a Stop's trigger too");
    for (tick, price) in [
        (1.0, "100"),
        (1.0, "101"),
        (0.05, "100.15"),
        (0.05, "0.15"),
        (0.1, "100.3"),
        (0.1, "65432.1"),
        (0.25, "99.75"),
        (0.5, "1234567.5"),
        (0.01, "1234567.89"),
        (1e-8, "0.00001234"),
    ] {
        assert_matches!(
            ticket::order(1, &limit(price, false), &on(tick)),
            Ok(TradeWindowAction::Place { .. }),
            "{price} on a {tick} tick is on the grid: {:?}",
            ticket::order(1, &limit(price, false), &on(tick))
        );
    }
    // On screen: the Buy button refuses and says why, and nothing is sent.
    let mut h = settled(Scene { grid: Grid { tick_size: Some(1.0), ..BTC }, ..demo() });
    set(&mut h, |s| {
        s.price = "100.4".to_string();
        s.take_profit_stop_loss = true;
    });
    assert!(disabled(&h, "Buy 0.010 limit"));
    click(&mut h, "Buy 0.010 limit");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    let why = "100.4 is not on the 1 tick: use a multiple of 1.";
    assert!(says_on_hover(&mut h, "Buy 0.010 limit", why));
}

/// Fix round 1, I-4: a size typed in the quote currency converts at the price EACH order is placed
/// at: a market buy at the ask, a market sell at the bid, a limit at its own price, and never at
/// the last trade (95 here, which would buy 10.526). The button names what the order sends.
#[test]
fn a_quote_size_converts_at_the_price_each_order_is_placed_at() {
    let mut h = settled(Scene { last: Some(95.0), ..demo() });
    set(&mut h, |s| {
        s.unit = TradeWindowSizingUnit::Quote;
        s.size = "1000".to_string();
        s.order_type = TradeWindowTicketKind::Market;
    });
    for (button, side, want) in [("Buy 10.000 market", 1, 10.0), ("Sell 10.010 market", -1, 10.01)]
    {
        click(&mut h, button);
        match places(h.state()).as_slice() {
            [
                TradeWindowAction::Place {
                    side: s,
                    order_type: TradeWindowTicketKind::Market,
                    quantity,
                    ..
                },
            ] => {
                assert!(*s == side && (quantity - want).abs() < 1e-9, "{button}: {quantity}");
            }
            other => panic!("{button}: one market order, got {other:?}"),
        }
        h.state_mut().emitted.clear();
    }
    set(&mut h, |s| {
        s.order_type = TradeWindowTicketKind::Limit;
        s.price = "80".to_string();
    });
    click(&mut h, "Buy 12.500 limit");
    match places(h.state()).as_slice() {
        [TradeWindowAction::Place { price: Some(p), quantity, .. }] => {
            assert!((p - 80.0).abs() < 1e-9 && (quantity - 12.5).abs() < 1e-9, "{p} {quantity}");
        }
        other => panic!("one limit at 80, got {other:?}"),
    }
}

/// Fix round 1, Minors 3 and 4: a size too large to be a number of lots, and a size under the
/// instrument's smallest order, are refused with a reason naming what is wrong; neither reaches a
/// `Place`.
#[test]
fn a_size_too_large_or_under_the_smallest_order_is_refused() {
    let b = book();
    let btc = inputs_on(&b, BTC);
    let sized = |size: &str| {
        let size = size.to_string();
        state_with(move |s| {
            s.size = size;
            s.price = "100".to_string();
        })
    };
    assert_eq!(trade_window::sizing::floor_to_lot(1e306, 0.001), 0.0, "never infinite lots");
    match ticket::order(1, &sized("1e306"), &btc) {
        Err(why) => assert!(why.contains("too large"), "{why}"),
        other => panic!("refused, got {other:?}"),
    }
    let min = inputs_on(&b, Grid { min_qty: Some(0.005), ..BTC });
    assert_eq!(
        ticket::order(1, &sized("0.002"), &min),
        Err("Below the smallest order size: enter at least 0.005 BTC.".to_string())
    );
    assert_matches!(ticket::order(1, &sized("0.005"), &min), Ok(TradeWindowAction::Place { .. }));
}

/// TP/SL on a limit Buy sends ONE bracket entry with both exits on the tick, and the ticket shows
/// the prices it will send them at, for a buy and for a sell (spec §3.6, pre-flight Minor 16).
#[test]
fn take_profit_stop_loss_on_a_limit_buy_sends_exits_on_the_tick_and_shows_their_prices() {
    let mut h = settled(demo());
    set(&mut h, |s| {
        s.price = "100".to_string();
        s.take_profit_stop_loss = true;
    });
    assert!(h.query_by_label("Buy: TP 100.5 · SL 99.7").is_some(), "a buy's exits are shown");
    assert!(h.query_by_label("Sell: TP 99.5 · SL 100.3").is_some(), "a sell's exits are shown");
    h.get_by_label_contains("Buy 0.010 limit").click();
    h.run();
    match places(h.state()).as_slice() {
        [
            TradeWindowAction::Place {
                side: 1,
                order_type: TradeWindowTicketKind::Limit,
                exits: Some(e),
                ..
            },
        ] => {
            assert!(
                (e.take_profit - 100.5).abs() < 1e-9 && (e.stop_loss - 99.7).abs() < 1e-9,
                "{e:?}"
            );
        }
        other => panic!("one bracket entry, got {other:?}"),
    }
}

/// A bracket is BOTH legs or no order. With TP/SL on, a leg that is blank, not a number or not
/// above zero refuses the order with a reason and sends NOTHING; the old ticket sent exactly that
/// order without its exits, as a plain one (carry from the bracket review). The positive control
/// is the test above: both legs set, the bracket goes.
#[test]
fn take_profit_stop_loss_with_a_leg_missing_sends_nothing_not_a_plain_order() {
    for (what, take_profit, stop_loss) in [
        ("TP blank", "", "0.3"),
        ("SL blank", "0.5", ""),
        ("SL not a number", "0.5", "abc"),
        ("SL zero", "0.5", "0"),
        ("SL below zero", "0.5", "-0.3"),
    ] {
        let mut h = settled(demo());
        set(&mut h, |s| {
            s.price = "100".to_string();
            s.take_profit_stop_loss = true;
            s.take_profit_text = take_profit.to_string();
            s.stop_loss_text = stop_loss.to_string();
        });
        assert!(disabled(&h, "Buy 0.010 limit"), "{what}: Buy is refused");
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: nothing is held either");
        assert!(
            says_on_hover(&mut h, "Buy 0.010 limit", ticket::TAKE_PROFIT_STOP_LOSS_LEGS_WHY),
            "{what}"
        );
    }
}

/// The money path called directly: each refusal carries its reason, and none of them is an order
/// sent without what the trader asked for.
#[test]
fn the_order_refuses_with_its_reason_and_never_drops_an_exit() {
    let b = book();
    let btc = inputs_on(&b, BTC);
    let limit_at_100 = |s: &mut TradeWindowState| {
        s.size = "0.010".to_string();
        s.price = "100".to_string();
    };
    // Both legs set: a bracket entry (the positive control).
    let both = state_with(|s| {
        limit_at_100(s);
        s.take_profit_stop_loss = true;
    });
    match ticket::order(1, &both, &btc) {
        Ok(TradeWindowAction::Place {
            exits: Some(TradeWindowTicketExits { take_profit, stop_loss }),
            ..
        }) => {
            assert!((take_profit - 100.5).abs() < 1e-9 && (stop_loss - 99.7).abs() < 1e-9);
        }
        other => panic!("a bracket entry, got {other:?}"),
    }
    // One leg missing, not a number or not above zero: refused.
    for (take_profit, stop_loss) in
        [("", "0.3"), ("0.5", ""), ("0.5", "abc"), ("0.5", "0"), ("abc", "-1")]
    {
        let s = state_with(|s| {
            limit_at_100(s);
            s.take_profit_stop_loss = true;
            s.take_profit_text = take_profit.to_string();
            s.stop_loss_text = stop_loss.to_string();
        });
        assert_eq!(
            ticket::order(1, &s, &btc),
            Err(ticket::TAKE_PROFIT_STOP_LOSS_LEGS_WHY.to_string()),
            "{take_profit:?} {stop_loss:?}"
        );
    }
    // No tick grid: an exit would sit off the grid, so no bracket and no order.
    let no_tick = inputs_on(&b, Grid { tick_size: None, ..BTC });
    assert_eq!(
        ticket::order(1, &both, &no_tick),
        Err(ticket::TAKE_PROFIT_STOP_LOSS_ROOM_WHY.to_string())
    );
    // TP/SL ticked where it is refused (a Stop entry) refuses the ORDER (fix round 1, I-3): it is
    // never sent without the exits the trader set. POSITIVE CONTROL: the same Stop with TP/SL off
    // goes, plain.
    let stop = |take_profit_stop_loss| {
        state_with(move |s| {
            limit_at_100(s);
            s.take_profit_stop_loss = take_profit_stop_loss;
            s.order_type = TradeWindowTicketKind::Stop;
        })
    };
    assert_eq!(
        ticket::order(1, &stop(true), &btc),
        Err(ticket::take_profit_stop_loss_refusal(ticket::TAKE_PROFIT_STOP_LOSS_STOP_WHY))
    );
    assert_matches!(
        ticket::order(1, &stop(false), &btc),
        Ok(TradeWindowAction::Place { order_type: TradeWindowTicketKind::Stop, exits: None, .. })
    );
    // No lot (pre-flight I18): refused, and the reason never prints the lot as "(0)".
    let no_lot = ticket::order(1, &state_with(limit_at_100), &inputs_on(&b, NO_LOT));
    assert_eq!(no_lot, Err(ticket::no_lot_reason("BTCUSDT")));
    assert!(!ticket::no_lot_reason("BTCUSDT").contains("(0)"));
}
