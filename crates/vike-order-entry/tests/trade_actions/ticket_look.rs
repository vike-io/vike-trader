//! The v3 ticket's look, as structure: the owner's design read off the accessibility tree.

use super::*;

/// The rects of the controls named `labels`, in order.
fn rects_of(h: &Harness<'_, Fixture>, labels: &[&str]) -> Vec<egui::Rect> {
    labels.iter().map(|l| h.get_by_label(l).rect()).collect()
}

/// Whether `row` is as many buttons of one width, on one line, a `gap` apart.
fn equal_buttons(row: &[egui::Rect], gap: f32) -> bool {
    row.windows(2).all(|p| {
        (p[0].width() - p[1].width()).abs() <= 0.6
            && (p[0].center().y - p[1].center().y).abs() <= 0.6
            && (p[1].min.x - p[0].max.x - gap).abs() <= 0.6
    })
}

/// The owner's v3 design: every row of the full ticket that is a row of choices is EQUAL buttons —
/// the three order types, the five quick sizes, the four shares, Buy and Sell, Close and Reverse —
/// and the ones across the ticket (all but Close and Reverse, which are across the card) end where
/// the order types do: the same left edge, the same right edge.
///
/// In the default look, Standard: the ticket grows with the text (the owner's pick 3b, 2026-10-05),
/// so five quick sizes still fit one row. At a fixed 260 pt they wrapped three and two from Standard
/// up, which is why this test was pinned to Small for a few hours.
#[test]
fn every_row_of_choices_in_the_full_ticket_is_equal_buttons_across_it() {
    let mut h = settled(demo());
    set(&mut h, |s| s.price = "99.5".to_string());
    let gap = vike_ui_theme::components::ThemeTokens::of(&h.ctx).metrics.gap;
    let types = rects_of(&h, &["Limit", "Market", "Stop"]);
    let across = [
        ("quick sizes", rects_of(&h, &["0.001", "0.005", "0.010", "0.050", "0.100"])),
        ("shares", rects_of(&h, &["10%", "25%", "50%", "100%"])),
        ("Buy and Sell", rects_of(&h, &["Buy 0.010 limit", "Sell 0.010 limit"])),
    ];
    assert!(equal_buttons(&types, gap), "the order types: {types:?}");
    let close = rects_of(&h, &["Close", "Reverse"]);
    assert!(equal_buttons(&close, gap), "Close and Reverse: {close:?}");
    let (left, right) = (types[0].min.x, types[2].max.x);
    for (what, row) in &across {
        assert!(equal_buttons(row, gap), "the {what}: {row:?}");
        let (l, r) = (row[0].min.x, row[row.len() - 1].max.x);
        assert!(
            (l - left).abs() <= 0.6 && (r - right).abs() <= 0.6,
            "the {what} span the ticket as the order types do: {l}..{r} against {left}..{right}"
        );
    }
    // The card is inside the ticket's edges, its buttons across the card.
    assert!(close[0].min.x >= left && close[1].max.x <= right, "{close:?} within {left}..{right}");
}

/// The toggles are BUTTONS that read pressed — Reduce only, TP/SL, Post only, the chosen order
/// type, the padlock — not check boxes; Leverage is a plain button, disabled, with no pressed state
/// to announce.
#[test]
fn the_toggles_are_buttons_that_read_pressed_and_the_greyed_ones_do_not() {
    let mut h = settled(demo());
    let state = |h: &Harness<'_, Fixture>, name: &str| {
        let n = h.get_by_label(name).accesskit_node();
        (n.role(), n.toggled())
    };
    assert_eq!(state(&h, "Reduce only"), (Role::Button, Some(Toggled::False)));
    assert_eq!(state(&h, "TP/SL"), (Role::Button, Some(Toggled::False)));
    click(&mut h, "Reduce only");
    assert!(h.state().state.reduce_only, "the click turned it on");
    assert_eq!(state(&h, "Reduce only"), (Role::Button, Some(Toggled::True)));
    click(&mut h, "Reduce only");
    assert!(!h.state().state.reduce_only);
    click(&mut h, "TP/SL");
    assert!(h.state().state.take_profit_stop_loss, "the click turned TP/SL on");
    assert_eq!(state(&h, "TP/SL"), (Role::Button, Some(Toggled::True)));
    // Exactly one order type reads pressed, the one chosen.
    let words = |ty| match ty {
        TradeWindowTicketKind::Limit => "Limit",
        TradeWindowTicketKind::Market => "Market",
        TradeWindowTicketKind::Stop => "Stop",
    };
    for chosen in
        [TradeWindowTicketKind::Market, TradeWindowTicketKind::Stop, TradeWindowTicketKind::Limit]
    {
        click(&mut h, words(chosen));
        assert_eq!(h.state().state.order_type, chosen);
        for ty in [
            TradeWindowTicketKind::Limit,
            TradeWindowTicketKind::Market,
            TradeWindowTicketKind::Stop,
        ] {
            let want = if ty == chosen { Toggled::True } else { Toggled::False };
            assert_eq!(state(&h, words(ty)), (Role::Button, Some(want)), "{ty:?} with {chosen:?}");
        }
    }
    // Leverage is a plain greyed button with no pressed state to announce; Post only is a toggle
    // even where it is greyed (this window's venue has no post-only order): it announces "not
    // pressed", and `post_only_is_a_live_toggle_that_makes_the_order_post_only` presses it.
    assert_eq!(state(&h, "Leverage"), (Role::Button, None), "Leverage is a plain button");
    assert!(disabled(&h, "Leverage"), "Leverage is greyed");
    assert_eq!(state(&h, "Post only"), (Role::Button, Some(Toggled::False)));
    assert!(disabled(&h, "Post only"), "and greyed here");
}

/// The position card: the side, the size at the average price and the P/L on ONE line, the P/L at
/// the card's right, Close and Reverse under them across the card's width. FLAT, the card says so
/// and its buttons are drawn, as wide, greyed.
#[test]
fn the_position_card_puts_the_pl_beside_the_facts_and_its_buttons_across_it() {
    let h = settled(demo());
    let (side, facts) = (h.get_by_label("LONG").rect(), h.get_by_label("0.050 @ 99.0").rect());
    let pl = h.get_by_label("+0.05 USDT").rect();
    assert!(
        (facts.center().y - pl.center().y).abs() <= 1.0
            && (side.center().y - pl.center().y).abs() <= 3.0,
        "one line: {side:?} {facts:?} {pl:?}"
    );
    assert!(side.max.x <= facts.min.x + 0.6 && facts.max.x <= pl.min.x, "left to right");
    let (close, reverse) = (h.get_by_label("Close").rect(), h.get_by_label("Reverse").rect());
    assert!(close.min.y >= facts.max.y && reverse.min.y >= facts.max.y, "buttons under the facts");
    assert!((pl.max.x - reverse.max.x).abs() <= 1.0, "the P/L ends where the card's buttons do");
    let flat = settled(Scene { position: None, ..demo() });
    assert!(flat.query_by_label("FLAT").is_some());
    assert!(flat.query_by_label("No position on this account").is_some());
    assert!(disabled(&flat, "Close") && disabled(&flat, "Reverse"), "nothing to close or reverse");
    let widths = rects_of(&flat, &["Close", "Reverse"]);
    assert!((widths[0].width() - widths[1].width()).abs() <= 0.6, "{widths:?}");
}

/// The exits a BUY would carry sit beside the TP/SL toggle as the design writes them, grey while
/// TP/SL is off and in the market's colours once it is on; with the distances blank there are no
/// exits to name, and dashes stand where they would be.
#[test]
fn the_take_profit_stop_loss_caption_names_the_exits_beside_the_toggle_grey_until_it_is_on() {
    let mut h = settled(demo());
    set(&mut h, |s| s.price = "100".to_string());
    let t = vike_ui_theme::components::ThemeTokens::of(&h.ctx);
    let colour = |h: &Harness<'_, Fixture>, text: &str| {
        painted(h).into_iter().find(|p| p.text == text).map(|p| p.sections[0].1)
    };
    assert_eq!(colour(&h, "TP 100.5"), Some(t.theme.text_tertiary), "off: grey");
    assert_eq!(colour(&h, "SL 99.7"), Some(t.theme.text_tertiary), "off: grey");
    let (toggle, take_profit) = (h.get_by_label("TP/SL").rect(), h.get_by_label("TP 100.5").rect());
    assert!(take_profit.min.x >= toggle.max.x, "beside the toggle, on its right");
    assert!((take_profit.center().y - toggle.center().y).abs() <= 1.0, "on its row");
    set(&mut h, |s| s.take_profit_stop_loss = true);
    assert_eq!(colour(&h, "TP 100.5"), Some(t.market.up_text), "on: the buy's take-profit");
    assert_eq!(colour(&h, "SL 99.7"), Some(t.market.down_text), "on: its stop-loss");
    set(&mut h, |s| s.stop_loss_text.clear());
    assert!(h.query_by_label("TP —").is_some() && h.query_by_label("SL —").is_some(), "dashes");
}

/// The compact ticket leads with the position strip: the facts and, under them, the P/L at the
/// left; Close and Reverse, in that order, at the right; and the size row names its field. The
/// owner's ruling that Join stays out holds there too (`neither_ticket_offers_join_and_bid_and_ask_only_type_the_price`).
#[test]
fn the_compact_ticket_leads_with_the_position_strip_and_names_its_size() {
    let mut h = harness_at(demo(), trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
    h.run();
    let facts = h.get_by_label_contains("LONG").rect();
    let pl = h.get_by_label("+0.05 USDT").rect();
    assert!(pl.min.y >= facts.max.y - 1.0, "the P/L is under the facts: {facts:?} {pl:?}");
    assert!((pl.min.x - facts.min.x).abs() <= 0.6, "and at their left edge");
    let (close, reverse) = (h.get_by_label("Close").rect(), h.get_by_label("Reverse").rect());
    assert!(close.max.x <= reverse.min.x && (close.center().y - reverse.center().y).abs() <= 0.6);
    assert!(close.min.x >= facts.min.x && close.min.x >= pl.max.x, "buttons at the strip's right");
    let size = h.get_by_label("Size").rect();
    assert!(size.min.y > reverse.max.y, "the size row is under the strip: {size:?}");
}
