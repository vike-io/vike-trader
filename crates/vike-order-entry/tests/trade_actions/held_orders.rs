//! The final fix wave (FW1): the confirm's words, a held order dropped or kept as its price and
//! position change, the market's price.

use super::*;

/// The owner's decision of 2026-10-03 (card 5 (ii)): neither ticket offers Join, the one-click
/// order at the best bid or ask. The full ticket's Bid and Ask stay: they only TYPE the best price
/// into the price field, and send nothing.
#[test]
fn neither_ticket_offers_join_and_bid_and_ask_only_type_the_price() {
    for (size, panel) in [
        (trade::BESIDE_SIZE, TradeWindowTicketPlace::Beside),
        (trade::UNDER_SIZE, TradeWindowTicketPlace::Under),
    ] {
        let mut h = harness_at(demo(), size, panel);
        h.run();
        let joins: Vec<String> = h
            .query_all_by_label_contains("Join")
            .filter_map(|n| n.accesskit_node().label())
            .collect();
        assert!(joins.is_empty(), "{panel:?}: {joins:?}");
    }
    let mut h = settled(demo());
    click(&mut h, "Bid");
    assert_eq!(h.state().state.price, "99.9", "Bid types the best bid");
    click(&mut h, "Ask");
    assert_eq!(h.state().state.price, "100.0", "Ask types the best ask");
    assert!(h.state().emitted.is_empty(), "and neither sends anything: {:?}", h.state().emitted);
}

/// Final review A, minor 2: the confirm prompt says "reduce only" when the order it holds is: the
/// order keeps the flag it was made with, whatever the box shows by the time the trader confirms.
/// CONTROL: an order that is not reduce-only does not say it.
#[test]
fn the_confirm_prompt_says_reduce_only_when_the_order_is() {
    for reduce in [true, false] {
        let mut h = settled(live());
        set(&mut h, |s| {
            s.price = "100".to_string();
            s.reduce_only = reduce;
        });
        click(&mut h, "Buy 0.010 limit");
        assert_matches!(
            &h.state().state.held, Some(TradeWindowAction::Place { reduce_only, .. }) if *reduce_only == reduce,
            "{reduce}: held {:?}",
            h.state().state.held
        );
        let strip = labels_in(&h, "Status strip");
        assert_eq!(strip.iter().any(|l| l.contains("reduce only")), reduce, "{reduce}: {strip:?}");
    }
}

/// Final review A, minor 3 (MONEY PATH): a held Close or Reverse names the side and size it sends
/// (the dispatcher sizes it from the position at the confirm: the position's size, twice it for a
/// Reverse) and is DROPPED, with a note, the moment the position's size changes under it: a
/// Reverse held while a resting buy filled would have sent twice the NEW position. CONTROL: a tick
/// of the market moves the P/L, not the size, and keeps it.
#[test]
fn a_held_close_or_reverse_names_its_size_and_is_dropped_when_the_position_changes() {
    for (button, words) in [
        ("Close", "Close the position: sell 0.050 at market"),
        ("Reverse", "Reverse the position: sell 0.100 at market"),
    ] {
        let mut h = settled(live());
        click(&mut h, button);
        assert!(h.state().state.held.is_some(), "{button}: held for a confirm");
        let strip = labels_in(&h, "Status strip");
        assert!(strip.iter().any(|l| l.contains(words)), "{button}: {strip:?}");
        h.state_mut().scene.position = Some(TradeWindowPosition { unrealized_pnl: 0.07, ..OPEN });
        h.run();
        assert!(h.state().state.held.is_some(), "{button}: CONTROL: a tick keeps it");
        assert!(h.state().emitted.is_empty(), "{button}: {:?}", h.state().emitted);
        h.state_mut().scene.position = Some(TradeWindowPosition { size: 0.08, ..OPEN });
        h.run();
        assert!(h.state().state.held.is_none(), "{button}: dropped");
        assert!(h.query_by_label("Place").is_none(), "{button}: nothing left to confirm");
        match h.state().emitted.as_slice() {
            [TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Error, text }] => {
                assert!(text.starts_with("Not sent:") && text.contains("position"), "{text}");
            }
            other => panic!("{button}: one error note, got {other:?}"),
        }
    }
}

/// Final review A, minor 4: the one-click padlock lets go of a held order AND says so, "Not
/// sent.", as the strip's Cancel does. CONTROL: with nothing held, the padlock says nothing.
#[test]
fn the_padlock_letting_go_of_a_held_order_says_not_sent() {
    let mut h = settled(live());
    h.get_by_label_contains("One-click trading").click();
    h.run();
    assert!(h.state().state.one_click, "CONTROL: the click turned it on");
    assert!(h.state().emitted.is_empty(), "CONTROL: nothing held, nothing said");
    let mut h = settled(live());
    set(&mut h, |s| s.price = "99.5".to_string());
    click(&mut h, "Buy 0.010 limit");
    assert!(h.state().state.held.is_some(), "held for a confirm");
    h.state_mut().emitted.clear();
    h.get_by_label_contains("One-click trading").click();
    h.run();
    assert!(h.state().state.held.is_none(), "let go");
    assert_eq!(
        h.state().emitted,
        [TradeWindowAction::Note {
            kind: TradeWindowStatusLineKind::Info,
            text: "Not sent.".to_string()
        }]
    );
}

/// F2 review minor 3: the full ticket's max never hides a KNOWN zero behind the dash that means
/// "not known": no buying power, or less than one lot's worth, says so. The dash is for a max
/// nobody can compute (no price to divide by); a buying power nobody gave draws no max at all.
#[test]
fn the_max_says_a_known_zero_and_a_dash_only_for_what_is_not_known() {
    for (what, scene, want) in [
        (
            "no buying power",
            Scene { buying_power: Some(0.0), ..demo() },
            "max 0 BTC · no buying power",
        ),
        ("a debt", Scene { buying_power: Some(-5.0), ..demo() }, "max 0 BTC · no buying power"),
        (
            "under one lot",
            Scene { buying_power: Some(0.05), ..demo() },
            "max 0 BTC · under one lot",
        ),
        ("CONTROL: enough", demo(), "max 100.000 BTC"),
        ("no price", Scene { book: false, last: None, ..demo() }, "max — BTC"),
    ] {
        let h = settled(scene);
        let ticket = labels_in(&h, "Order ticket");
        assert!(ticket.iter().any(|l| l == want), "{what}: {want:?} in {ticket:?}");
    }
    let h = settled(Scene { buying_power: None, ..demo() });
    let ticket = labels_in(&h, "Order ticket");
    assert!(!ticket.iter().any(|l| l.starts_with("max ")), "not known: no max: {ticket:?}");
}

/// The owner's decision of 2026-10-03 (round 2, item 9): a Stop whose trigger is on the wrong side
/// of the market — a buy stop below the ask, a sell stop above the bid — triggers at once. The
/// ticket WARNS before the click, on a line of its own with the real numbers, and the confirm
/// prompt says the same words; the order is NOT refused (venues differ on whether they fire it or
/// reject it), and it is sent. CONTROL: a side whose trigger is on the right side warns nothing.
#[test]
fn a_stop_on_the_wrong_side_of_the_market_is_warned_and_still_sent() {
    let buy_warning = "Stop BUY 98.0 is below the market 100.0: it triggers at once.";
    let sell_warning = "Stop SELL 101.0 is above the market 99.9: it triggers at once.";
    let mut h = settled(live());
    set(&mut h, |s| {
        s.order_type = TradeWindowTicketKind::Stop;
        s.price = "101".to_string();
    });
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == sell_warning), "a sell stop above the bid: {ticket:?}");
    assert!(!ticket.iter().any(|l| l.starts_with("Stop BUY")), "CONTROL: {ticket:?}");
    set(&mut h, |s| s.price = "98".to_string());
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == buy_warning), "the ticket warns first: {ticket:?}");
    assert!(!ticket.iter().any(|l| l.starts_with("Stop SELL")), "CONTROL: {ticket:?}");
    assert!(!disabled(&h, "Buy 0.010 stop"), "a warning, not a refusal");
    click(&mut h, "Buy 0.010 stop");
    assert!(h.state().state.held.is_some(), "held for a confirm");
    let strip = labels_in(&h, "Status strip");
    assert!(strip.iter().any(|l| l == buy_warning), "the confirm says the same words: {strip:?}");
    click(&mut h, "Place");
    match places(h.state()).as_slice() {
        [
            TradeWindowAction::Place {
                side: 1,
                order_type: TradeWindowTicketKind::Stop,
                price: Some(p),
                ..
            },
        ] => {
            assert!((p - 98.0).abs() < 1e-9, "{p}");
        }
        other => panic!("one buy stop at 98, got {other:?}"),
    }
}

/// What the node says of a held order is on the prompt, in the warning's colour, before the Place it
/// asks about: the quantity the order goes out with, or that it would be refused. It stops nothing: the
/// node judges the send. A long refusal is whole, because a door's refusal lists the books to choose
/// from in its tail; CONTROL: nothing said adds no line.
#[test]
fn what_the_node_says_of_a_held_order_is_on_its_prompt() {
    let note = "quantity 15 goes out as 20, the venue's step is 10";
    let held = |says| {
        let mut h = settled(Scene { held_says: says, ..live() });
        set(&mut h, |s| s.order_type = TradeWindowTicketKind::Limit);
        set(&mut h, |s| s.price = "99".to_string());
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().state.held.is_some(), "held for a confirm");
        h
    };
    let h = held(Some(note));
    let said = in_strip(&h);
    let line = said.iter().find(|p| p.text == note).expect("the note is its own line");
    assert_eq!(line.sections[0].1, ThemeStatus::Warning.color(), "{:?}", line.sections);
    assert_eq!(labels_in(&held(None), "Status strip").iter().filter(|l| *l == note).count(), 0);
    let mut h = held(Some(note));
    click(&mut h, "Place");
    assert_eq!(places(h.state()).len(), 1, "a note stops nothing");

    let refused = "The node would refuse this: venue `hyperliquid` runs more than one account on \
                   this node, and this order names none of them. It was REFUSED and nothing was \
                   sent. Name one of these books: hyperliquid/DEMO, hyperliquid/LAST";
    let h = held(Some(refused));
    let words: String = in_strip(&h).iter().map(|p| p.text.as_str()).collect::<Vec<_>>().join(" ");
    assert!(words.contains("hyperliquid/LAST"), "the tail is the books: {words}");
    assert!(in_strip(&h).iter().all(|p| !p.elided), "{words}");
}

/// ⚠ A refusal on the strip is whole however long: its two lines of Body text give way to as many
/// Caption lines as it takes, and the strip grows. A door's refusal lists the books in its tail, and a
/// line cut there tells the trader nothing to do. CONTROL: a long line that is not a refusal keeps its
/// two lines and its hover.
#[test]
fn a_long_refusal_on_the_strip_is_never_cut() {
    let long = "Not placed: venue `hyperliquid` runs more than one account on this node, and this \
                order names none of them. It was REFUSED and nothing was sent: an order or a close \
                goes only to the one account it names. Name one of these books: hyperliquid/DEMO, \
                hyperliquid/LAST";
    let on = |kind| {
        let mut h = harness_at(
            Scene { status: Some((kind, long)), ..demo() },
            trade::UNDER_SIZE,
            TradeWindowTicketPlace::Under,
        );
        h.run();
        h
    };
    let h = on(TradeWindowStatusLineKind::Error);
    let said = in_strip(&h);
    assert!(said.iter().any(|p| p.text == long && !p.elided), "{said:?}");
    let h = on(TradeWindowStatusLineKind::Info);
    let said = in_strip(&h);
    assert!(said.iter().any(|p| p.elided), "CONTROL: an Info line keeps its two lines: {said:?}");
}

/// The owner's decision of 2026-10-03 (round 2, item 10): a market Buy or Sell is REFUSED, with
/// the reason on the button, where there is no price for it at all — no side of the book to fill
/// against and no last price: such an order goes out blind, and the app's notional cap, which needs
/// a price, cannot hold it either. In both tickets. CONTROL: a last price alone is enough.
#[test]
fn a_market_order_with_no_book_and_no_last_price_is_refused() {
    let b = L2Book::new(0.1);
    let market = state_with(|s| {
        s.size = "0.010".to_string();
        s.order_type = TradeWindowTicketKind::Market;
    });
    let blind = TradeWindowInputs { last: None, ..inputs_on(&b, BTC) };
    for side in [1, -1] {
        assert_eq!(
            ticket::order(side, &market, &blind),
            Err(ticket::NO_MARKET_PRICE_WHY.to_string()),
            "{side}"
        );
    }
    assert_matches!(
        ticket::order(1, &market, &inputs_on(&b, BTC)),
        Ok(TradeWindowAction::Place { order_type: TradeWindowTicketKind::Market, .. }),
        "CONTROL: a last price is enough"
    );
    // PER SIDE (the FW1 review's pin): a one-sided book with no last price refuses only the side
    // it has no price for; the other side fills against the side that is there.
    let one_sided = |bids: &[L2BookLevel], asks: &[L2BookLevel]| {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, bids, asks);
        b
    };
    let bids = one_sided(&[L2BookLevel::new(99.9, 1.0)], &[]);
    let asks = one_sided(&[], &[L2BookLevel::new(100.0, 1.0)]);
    for (what, book, priced, blind_side) in
        [("bids only", &bids, -1, 1), ("asks only", &asks, 1, -1)]
    {
        let on = TradeWindowInputs { last: None, ..inputs_on(book, BTC) };
        assert_matches!(
            ticket::order(priced, &market, &on),
            Ok(TradeWindowAction::Place { order_type: TradeWindowTicketKind::Market, .. }),
            "{what}: the side with a price is sent"
        );
        assert_eq!(
            ticket::order(blind_side, &market, &on),
            Err(ticket::NO_MARKET_PRICE_WHY.to_string()),
            "{what}: the side with none is refused"
        );
    }
    let scene = || Scene { book: false, last: None, ..demo() };
    let mut h = settled(scene());
    set(&mut h, |s| s.order_type = TradeWindowTicketKind::Market);
    for button in ["Buy 0.010 market", "Sell 0.010 market"] {
        assert!(disabled(&h, button), "{button}");
        click(&mut h, button);
    }
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    assert!(says_on_hover(&mut h, "Buy 0.010 market", ticket::NO_MARKET_PRICE_WHY));
    let mut h = harness_at(scene(), trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
    h.run();
    for button in ["Buy 0.010 MKT", "Sell 0.010 MKT"] {
        assert!(disabled(&h, button), "under the ladder: {button}");
        click(&mut h, button);
    }
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
}

/// The FW1 review's pins on round 2, item 9: a trigger AT the market is on no wrong side and warns
/// nothing (the boundary: a buy stop at the ask, a sell stop at the bid); and with no book the
/// warning measures against the last price. CONTROL: the side the same trigger IS wrong for warns.
#[test]
fn a_stop_at_the_market_warns_nothing_and_with_no_book_the_last_price_is_the_market() {
    let mut h = settled(live());
    set(&mut h, |s| {
        s.order_type = TradeWindowTicketKind::Stop;
        s.price = "100.0".to_string();
    });
    let ticket = labels_in(&h, "Order ticket");
    assert!(!ticket.iter().any(|l| l.starts_with("Stop BUY")), "a buy stop at the ask: {ticket:?}");
    let control = "Stop SELL 100.0 is above the market 99.9: it triggers at once.";
    assert!(ticket.iter().any(|l| l == control), "CONTROL: {ticket:?}");
    set(&mut h, |s| s.price = "99.9".to_string());
    let ticket = labels_in(&h, "Order ticket");
    assert!(
        !ticket.iter().any(|l| l.starts_with("Stop SELL")),
        "a sell stop at the bid: {ticket:?}"
    );
    let control = "Stop BUY 99.9 is below the market 100.0: it triggers at once.";
    assert!(ticket.iter().any(|l| l == control), "CONTROL: {ticket:?}");

    let mut h = settled(Scene { book: false, last: Some(100.0), ..live() });
    set(&mut h, |s| {
        s.order_type = TradeWindowTicketKind::Stop;
        s.price = "98".to_string();
    });
    let ticket = labels_in(&h, "Order ticket");
    let below = "Stop BUY 98.0 is below the market 100.0: it triggers at once.";
    assert!(ticket.iter().any(|l| l == below), "no book: the last price: {ticket:?}");
    set(&mut h, |s| s.price = "101".to_string());
    let ticket = labels_in(&h, "Order ticket");
    let above = "Stop SELL 101.0 is above the market 100.0: it triggers at once.";
    assert!(ticket.iter().any(|l| l == above), "no book: the last price: {ticket:?}");
}

/// The FW1 review's check 5, the owner's round-2 item 10 applied to the position card: a REVERSE
/// with no price on the side it trades and no last price is refused, with the same stated reason
/// as a market Buy or Sell. Its second half opens a position the size of the current one, blind,
/// and the app's notional cap holds no request without a price. A CLOSE stays: it only takes away
/// what is held. What the trader sees: Reverse disabled, the reason on its hover; Close working.
/// In both tickets. CONTROL: a last price alone gives Reverse its price.
#[test]
fn a_reverse_with_no_price_is_refused_and_a_close_still_closes() {
    let scene = || Scene { book: false, last: None, ..demo() };
    for (panel, size) in [
        (TradeWindowTicketPlace::Beside, trade::BESIDE_SIZE),
        (TradeWindowTicketPlace::Under, trade::UNDER_SIZE),
    ] {
        let mut h = harness_at(scene(), size, panel);
        h.run();
        h.state_mut().emitted.clear();
        assert!(disabled(&h, "Reverse"), "{panel:?}: Reverse is disabled");
        click(&mut h, "Reverse");
        assert!(h.state().emitted.is_empty(), "{panel:?}: {:?}", h.state().emitted);
        assert!(says_on_hover(&mut h, "Reverse", ticket::NO_MARKET_PRICE_WHY), "{panel:?}");
        assert!(!disabled(&h, "Close"), "{panel:?}: Close stays");
        click(&mut h, "Close");
        assert_eq!(
            h.state().emitted,
            [TradeWindowAction::ClosePosition],
            "{panel:?}: Close closes"
        );
    }
    let h = settled(Scene { book: false, last: Some(100.0), ..demo() });
    assert!(!disabled(&h, "Reverse"), "CONTROL: a last price is enough");
}

/// What a trader at the confirm does: click Place, where the strip still offers it.
fn confirm(h: &mut Harness<'static, Fixture>) {
    if h.query_by_label("Place").is_some() {
        click(h, "Place");
    }
}

/// `emitted` is the one note a held order leaves when the price it needs went while it waited:
/// "Not sent: …", ending in [`ticket::NO_MARKET_PRICE_WHY`] word for word, the sentence the
/// refusal at the click says (one source for it).
fn assert_dropped_for_no_price(what: &str, emitted: &[TradeWindowAction]) {
    match emitted {
        [TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Error, text }] => assert!(
            text.starts_with("Not sent:") && text.ends_with(ticket::NO_MARKET_PRICE_WHY),
            "{what}: {text}"
        ),
        other => panic!("{what}: one error note, got {other:?}"),
    }
}

/// The FW4 dispatch, MONEY PATH (the owner's round-2 item 10, its HELD half): a REVERSE waiting for
/// its confirm is checked for a price at every frame, not only at the click. One-click is forced off
/// on LIVE, so there every Reverse waits in the strip; if the book and the last price go while it
/// waits, Place would send a market order nobody can price, which the app's notional cap cannot
/// hold. It is dropped with a note naming the cause, and the confirm sends nothing. CONTROL: while
/// the price is there it waits, and Place sends it.
#[test]
fn a_held_reverse_is_dropped_when_its_price_goes_and_kept_while_it_has_one() {
    for priced in [true, false] {
        let mut h = settled(live());
        click(&mut h, "Reverse");
        assert_eq!(h.state().state.held, Some(TradeWindowAction::Reverse), "{priced}: held");
        if !priced {
            h.state_mut().scene.book = false;
            h.state_mut().scene.last = None;
        }
        h.run();
        confirm(&mut h);
        let emitted = &h.state().emitted;
        if priced {
            assert_eq!(
                emitted,
                &[TradeWindowAction::Reverse],
                "CONTROL: priced, it waits and is sent"
            );
            continue;
        }
        assert!(
            !emitted.contains(&TradeWindowAction::Reverse),
            "the Reverse went out blind: {emitted:?}"
        );
        assert_eq!(h.state().state.held, None, "dropped");
        assert!(h.query_by_label("Place").is_none(), "nothing left to confirm");
        assert_dropped_for_no_price("a Reverse", emitted);
    }
}

/// FW4, the same for a held MARKET Buy or Sell, per side: on a book that loses one side, with no
/// last price, the order that fills against the side that went (a Buy takes the asks, a Sell the
/// bids) is dropped with the same note and Place sends nothing; the other side's order still has
/// its price, waits, and Place sends it as held (the CONTROL on the same book). A market order is
/// checked at the click (`ticket::order`) and again while it waits.
#[test]
fn a_held_market_order_whose_side_loses_its_price_is_dropped_and_the_other_side_kept() {
    // `one_side` keeps that side of the book: `+1` the bids alone, so the asks are gone.
    for (what, button, one_side, dropped) in [
        ("a Buy, the asks gone", "Buy 0.010 market", 1, true),
        ("CONTROL: a Sell, the asks gone", "Sell 0.010 market", 1, false),
        ("a Sell, the bids gone", "Sell 0.010 market", -1, true),
        ("CONTROL: a Buy, the bids gone", "Buy 0.010 market", -1, false),
    ] {
        let mut h = settled(Scene { last: None, ..live() });
        set(&mut h, |s| s.order_type = TradeWindowTicketKind::Market);
        click(&mut h, button);
        let held = h.state().state.held.clone().expect("held for a confirm");
        assert_matches!(
            held,
            TradeWindowAction::Place { order_type: TradeWindowTicketKind::Market, .. },
            "{what}: {held:?}"
        );
        h.state_mut().scene.one_side = Some(one_side);
        h.run();
        confirm(&mut h);
        if dropped {
            assert!(places(h.state()).is_empty(), "{what}: sent blind: {:?}", h.state().emitted);
            assert_eq!(h.state().state.held, None, "{what}: dropped");
            assert_dropped_for_no_price(what, &h.state().emitted);
        } else {
            assert_eq!(h.state().emitted, [held], "{what}: kept, and Place sends it as held");
        }
    }
}

/// A way a test puts an order in the strip for a confirm.
type Hold = fn(&mut Harness<'static, Fixture>);

/// FW4, what the price check leaves alone: a held CLOSE only takes away what is held (reduce-only),
/// so with no book and no last price it still waits and Place still sends it; a held LIMIT or STOP
/// carries its own price, so it waits too and Place sends it as held.
#[test]
fn a_held_close_limit_or_stop_is_kept_when_the_price_goes() {
    let holds: [(&str, Hold); 3] = [
        ("a Close", |h| click(h, "Close")),
        ("a limit", |h| {
            set(h, |s| s.price = "99.5".to_string());
            click(h, "Buy 0.010 limit");
        }),
        ("a stop", |h| {
            set(h, |s| {
                s.order_type = TradeWindowTicketKind::Stop;
                s.price = "101".to_string();
            });
            click(h, "Buy 0.010 stop");
        }),
    ];
    for (what, hold) in holds {
        let mut h = settled(live());
        hold(&mut h);
        let held = h.state().state.held.clone().expect("held for a confirm");
        h.state_mut().emitted.clear();
        h.state_mut().scene.book = false;
        h.state_mut().scene.last = None;
        h.run();
        assert_eq!(h.state().state.held.as_ref(), Some(&held), "{what}: it still waits");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        click(&mut h, "Place");
        assert_eq!(h.state().emitted, [held], "{what}: Place sends it as held");
    }
}
