//! The ladder's presses on screen: what the button went down on decides the order, a drag reprices
//! only what the venue can modify, a stale book places and moves nothing, and hovering a marker
//! lists its orders.

use super::super::super::columns::TradeWindowLadderColumn;
use super::super::super::markers::marker_lines;
use crate::trade_window::TradeWindowSizingUnit;

use super::*;

/// A STOP's pill is drawn with a dashed edge and no fill, a LIMIT's with a tinted fill and a
/// solid edge (the design's `.pill.stop`): read off the shapes, so a regression to a solid
/// outline is seen, not only a wrong constant. The dashes are many short line segments in
/// the side's colour; the limit's is one rounded rectangle.
#[test]
fn a_stop_pill_is_dashed_and_a_limit_pill_is_solid_on_screen() {
    fn flat(s: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| flat(s, out)),
            s => out.push(s.clone()),
        }
    }
    let shapes = |h: &Harness<'static, Scene>| {
        let mut out = Vec::new();
        for c in &h.output().shapes {
            flat(&c.shape, &mut out);
        }
        out
    };
    let dashes = |h: &Harness<'static, Scene>, ink: egui::Color32| {
        shapes(h)
            .iter()
            .filter(|s| matches!(s, egui::Shape::LineSegment { stroke, .. } if stroke.color == ink))
            .count()
    };
    let tinted = |h: &Harness<'static, Scene>, fill: egui::Color32| {
        shapes(h).iter().any(|s| matches!(s, egui::Shape::Rect(r) if r.fill == fill))
    };
    let capabilities = vike_venues::venue_caps::BINANCE;
    let t = ThemeTokens::from_appearance(&ThemeAppearance::default());
    let (up, tint) = (t.market.up, with_alpha(t.market.up, trade::PILL_FILL_ALPHA));

    let limit = settled(Setup {
        orders: vec![order("l", 1, 98.0, 0.01, false)],
        capabilities,
        ..Setup::default()
    });
    assert!(tinted(&limit, tint), "a limit pill is filled");
    assert_eq!(dashes(&limit, up), 0, "and its edge is not dashed");

    let stop = settled(Setup {
        orders: vec![order("s", 1, 98.0, 0.01, true)],
        capabilities,
        ..Setup::default()
    });
    assert!(!tinted(&stop, tint), "a stop pill is hollow");
    assert!(dashes(&stop, up) >= 8, "and its edge is dashes: {}", dashes(&stop, up));
}

/// The design's rows, read off what is painted: the LAST price is the text colour in the
/// semibold weight and every other rung the secondary grey in the regular one; the outlined
/// row's own edge is the boundary between the asks and the bids, so no line in the analysis
/// colour is drawn between them (the DOM's spread line is gone); and the Vol caption, the
/// column and the dashes are the caption grey.
#[test]
fn the_last_price_is_semibold_and_no_spread_line_is_drawn() {
    let h = settled(Setup::default());
    let (_, t) = h.state().drawn.expect("the ladder drew");
    let mut font_of = std::collections::HashMap::new();
    let mut segments = Vec::new();
    for c in &h.output().shapes {
        match &c.shape {
            egui::Shape::Text(s) => {
                let f = &s.galley.job.sections[0].format;
                font_of.insert(s.galley.text().to_string(), (f.font_id.family.clone(), f.color));
            }
            egui::Shape::LineSegment { stroke, .. } => segments.push(stroke.color),
            _ => {}
        }
    }
    let semibold = egui::FontFamily::Name(vike_ui_theme::fonts::MONO_SEMIBOLD.into());
    // MID is 99.5: the rows centre on 100, the best ask, which is also the last row.
    assert_eq!(font_of["100"], (semibold.clone(), t.theme.text));
    assert_eq!(font_of["101"], (egui::FontFamily::Monospace, t.theme.text_secondary));
    assert_eq!(font_of["99"], (egui::FontFamily::Monospace, t.theme.text_secondary));
    assert!(!segments.contains(&t.theme.analysis_line), "a spread line: {segments:?}");
}

/// A drag the ladder did not see end — `state.drag` with no button down on the ladder — is
/// dropped, so it can never reprice later; and a click after it trades.
#[test]
fn a_drag_the_ladder_did_not_see_end_is_dropped() {
    let mut h = settled(Setup::default());
    h.state_mut().state.drag = Some(vec!["gone".to_string()]);
    h.run();
    assert_eq!(h.state().state.drag, None);
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 98.0);
    assert!(place(1, TradeWindowTicketKind::Limit, 98.0, &got), "{got:?}");
}

/// Review fix 2: a size typed in the quote currency converts at the CLICKED price, the price
/// the order is placed at — not at the last price (which bought 9.849 here, not 10) — and it
/// still converts with no last price at all (where it placed nothing).
#[test]
fn a_quote_size_converts_at_the_clicked_price() {
    for last in [Some(MID), None] {
        let mut h = settled(Setup { last, ..Setup::default() });
        h.state_mut().state.unit = TradeWindowSizingUnit::Quote;
        h.state_mut().state.size = "980".to_string();
        let got = click(&mut h, TradeWindowLadderColumn::Bid, 98.0);
        assert!(
            place_quantity(1, TradeWindowTicketKind::Limit, 98.0, 10.0, &got),
            "{last:?}: {got:?}"
        );
        let got = click(&mut h, TradeWindowLadderColumn::Bid, 101.0);
        assert!(
            place_quantity(1, TradeWindowTicketKind::Stop, 101.0, 9.702, &got),
            "{last:?}: {got:?}"
        );
    }
}

/// Review fix 3: a click trades the row that was under the pointer when the button went DOWN.
/// The market moves one row while it is down; the rows hold still until the release, and the
/// click is resolved against them. ⚠ The CONTROL comes after: with the button up the rows
/// follow the market, so the same point is then the next row — the move did shift the rows.
#[test]
fn a_click_trades_the_row_under_the_pointer_when_the_button_went_down() {
    let mut h = settled(Setup::default());
    let at = cell(&h, TradeWindowLadderColumn::Bid, 98.0);
    h.event(Event::PointerMoved(at));
    press(&h, at, true);
    h.step();
    h.state_mut().book = book_at(MID + 1.0);
    h.step();
    press(&h, at, false);
    h.run();
    let got = std::mem::take(&mut h.state_mut().emitted);
    assert!(
        place(1, TradeWindowTicketKind::Limit, 98.0, &got),
        "the price under the press: {got:?}"
    );

    let got = click_at(&mut h, at);
    assert!(place(1, TradeWindowTicketKind::Limit, 99.0, &got), "CONTROL: the rows moved: {got:?}");
}

/// One-click OFF — where a LIVE account starts — HOLDS a ladder click for a confirm
/// (`TradeWindowState::submit`) rather than sending it.
#[test]
fn a_ladder_click_on_a_live_account_is_held_for_a_confirm() {
    let mut h = settled(Setup { mode: Some(AccountTier::Live), ..Setup::default() });
    assert!(!h.state().state.one_click, "a LIVE account starts with one-click off");
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 98.0);
    assert!(got.is_empty(), "nothing is sent: {got:?}");
    let held = h.state().state.held.clone();
    assert!(place(1, TradeWindowTicketKind::Limit, 98.0, held.as_slice()), "held: {held:?}");
}

/// The row arithmetic saturates: under a tick so small that every price's key saturates `i64`,
/// the ladder still draws and takes a click without a panic (a debug build panics on an
/// overflow) — and that click sends no order: a saturated key names no price. This test used to
/// pass on a `Place` at the saturated key times the tick, about 9e-282, a price no row showed
/// and nobody clicked (final review A, minor 1).
#[test]
fn a_vanishing_tick_saturates_rather_than_overflowing() {
    let grid = Grid { tick_size: Some(1e-300), ..GRID };
    let mut h = settled(Setup { grid, ..Setup::default() });
    let at = cell(&h, TradeWindowLadderColumn::Bid, 98.0);
    let got = click_at(&mut h, at);
    assert!(rows_rect(&h).contains(at), "CONTROL: the click landed on the rows");
    let sent: Vec<&TradeWindowAction> = got
        .iter()
        .filter(|a| matches!(a, TradeWindowAction::Place { .. } | TradeWindowAction::Modify { .. }))
        .collect();
    assert!(sent.is_empty(), "no order at a price nobody clicked: {sent:?}");
}

/// A press, `change` to the scene while the button is held (each a frame of its own, as a
/// trader's hand gives them), then the release at the same point; what it emitted.
fn press_change_release(
    h: &mut Harness<'static, Scene>,
    at: Pos2,
    change: impl FnOnce(&mut Scene),
) -> Vec<TradeWindowAction> {
    h.state_mut().emitted.clear();
    h.event(Event::PointerMoved(at));
    press(h, at, true);
    h.step();
    change(h.state_mut());
    h.step();
    h.step();
    press(h, at, false);
    h.run();
    std::mem::take(&mut h.state_mut().emitted)
}

/// Final review A, I-1 (MONEY PATH): whether a click CANCELS or PLACES is decided where the
/// button went down, against the marker drawn there — never again at the release. The trader
/// presses the hollow marker of a sell stop at 98 to pull it, and the stop fills while the
/// button is down: it is gone from the orders at the release, and a release that looked again
/// found an empty Sell cell and sent a NEW sell (a stop at 98, below the bid) — an order that
/// opens a short. A press on a marker is a cancel, and a cancel of an order that has gone is a
/// harmless refusal. CONTROL: the stop stays, and the same press cancels it. And the reverse
/// race: a press on an EMPTY cell places, even where a marker appears under the held button
/// (a fast second click landing on the order the first click placed).
#[test]
fn a_press_on_a_marker_cancels_whatever_happens_to_its_orders_before_the_release() {
    let stop = order("s1", -1, 98.0, 0.01, true);
    for (what, fills) in [("CONTROL: the stop stays", false), ("the stop fills", true)] {
        let mut h = settled(Setup { orders: vec![stop.clone()], ..Setup::default() });
        let at = cell(&h, TradeWindowLadderColumn::Sell, 98.0);
        let got = press_change_release(&mut h, at, |s| {
            if fills {
                s.orders.clear();
            }
        });
        assert_eq!(got, [TradeWindowAction::Cancel(vec!["s1".to_string()])], "{what}");
        assert_eq!(h.state().state.held, None, "{what}: nothing held for a confirm");
    }
    let mut h = settled(Setup::default());
    let at = cell(&h, TradeWindowLadderColumn::Buy, 98.0);
    let got = press_change_release(&mut h, at, |s| {
        s.orders.push(order("b1", 1, 98.0, 0.01, false));
    });
    assert!(
        place(1, TradeWindowTicketKind::Limit, 98.0, &got),
        "a press on an empty cell places: {got:?}"
    );
}

/// Final review A, minor 1: a row at a price of zero or below names no price — none is drawn —
/// and takes no click and no drop, as the ticket refuses a price that is not above zero. Such a
/// row is on screen as soon as the ladder is scrolled under zero, or at a coarse grouping on a
/// cheap instrument; an order there is a venue's rejection, or, on paper, a stop that never
/// fires. CONTROL: on the same ladder, a row above zero is drawn, places and takes a drop.
#[test]
fn a_row_at_or_below_zero_names_no_price_and_takes_no_click_or_drop() {
    let setup = || Setup {
        orders: vec![order("b1", 1, 3.0, 0.01, false)],
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    };
    let mut h = settled(setup());
    h.state_mut().state.center = Some(1.0);
    h.run();
    let texts = painted(&h);
    assert!(texts.iter().any(|t| t == "2"), "CONTROL: the row at 2 names its price: {texts:?}");
    assert!(
        !texts.iter().any(|t| t == "0" || t.starts_with('-')),
        "no price at or below zero is drawn: {texts:?}"
    );
    for price in [0.0, -2.0] {
        let got = click(&mut h, TradeWindowLadderColumn::Bid, price);
        assert!(got.is_empty(), "a click at {price}: {got:?}");
        assert_eq!(h.state().state.held, None, "and nothing is held at {price}");
    }
    let got = click(&mut h, TradeWindowLadderColumn::Bid, 1.0);
    assert!(
        place(1, TradeWindowTicketKind::Limit, 1.0, &got),
        "CONTROL: a row above zero: {got:?}"
    );
    let got = drag(&mut h, TradeWindowLadderColumn::Buy, 3.0, -1.0);
    assert!(got.is_empty(), "a drop below zero moves nothing: {got:?}");
    let got = drag(&mut h, TradeWindowLadderColumn::Buy, 3.0, 2.0);
    let moved = TradeWindowAction::Modify { order_client_id: "b1".to_string(), new_price: 2.0 };
    assert_eq!(got, [moved], "CONTROL: a drop above zero reprices");
}

/// The owner's decision of 2026-10-03, card 1: hovering a marker lists the orders it stands
/// for — each one's side, type, size and price — and their summed size, because at the shipped
/// column widths a marker of several orders shows only its count, and its click cancels every
/// one of them. CONTROL: a marker of one order lists that order alone, and an empty cell lists
/// nothing.
#[test]
fn hovering_a_marker_lists_its_orders_and_their_summed_size() {
    let orders = vec![
        order("b1", 1, 98.0, 0.01, false),
        order("b2", 1, 98.0, 0.02, true),
        order("b3", 1, 96.0, 0.005, false),
    ];
    let mut h = settled(Setup { orders, ..Setup::default() });
    h.ctx.all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
    let hover = |h: &mut Harness<'static, Scene>, col, price| {
        h.hover_at(pos2(1.0, 1.0));
        h.run();
        let at = cell(h, col, price);
        h.hover_at(at);
        h.run();
        labels(h)
    };
    let two = hover(&mut h, TradeWindowLadderColumn::Buy, 98.0);
    for line in ["Buy limit 0.010 @ 98", "Buy stop 0.020 @ 98", "Total: 0.030 BTC in 2 orders"] {
        assert!(two.iter().any(|l| l == line), "{line:?} in {two:?}");
    }
    assert!(!two.iter().any(|l| l.contains("@ 96")), "only this row's orders: {two:?}");
    let one = hover(&mut h, TradeWindowLadderColumn::Buy, 96.0);
    for line in ["Buy limit 0.005 @ 96", "Total: 0.005 BTC in 1 order"] {
        assert!(one.iter().any(|l| l == line), "CONTROL: {line:?} in {one:?}");
    }
    assert!(!one.iter().any(|l| l.contains("@ 98")), "CONTROL: {one:?}");
    let none = hover(&mut h, TradeWindowLadderColumn::Buy, 97.0);
    assert!(!none.iter().any(|l| l.starts_with("Total:")), "an empty cell: {none:?}");
}

/// An order whose id has friendly words (decision 0270) lists them under its own line in the
/// hover; one without lists its line alone, exactly as before.
#[test]
fn a_markers_hover_lists_an_orders_words_under_its_line() {
    let words = "GUI (by hand) · no strategy · account #4 · opens or adds long";
    let said =
        TradeWindowLadderOrder { words: Some(words.into()), ..order("g", 1, 98.0, 0.01, false) };
    let plain = order("deadbeef17", 1, 98.0, 0.02, false);
    let lines = marker_lines(&[&said, &plain], 1.0, Some(0.001), "BTC");
    assert_eq!(
        lines,
        [
            "Buy limit 0.010 @ 98".to_string(),
            format!("  {words}"),
            "Buy limit 0.020 @ 98".into(),
            "Total: 0.030 BTC in 2 orders".into(),
        ]
    );
}

/// The owner's decision of 2026-10-03 (round 2, item 2): while the book is STALE the ladder
/// places no order and moves none — limit or stop is chosen against a top of book that may be
/// long gone, and a drop prices an order off it — but a marker still CANCELS: a cancel takes
/// risk away and names its orders, not a price. The hint line says why, and a refused click or
/// drop says so on the strip. CONTROL: on a live book the same click places and the same drag
/// reprices.
#[test]
fn a_stale_ladder_places_and_moves_nothing_but_a_marker_still_cancels() {
    let setup = || Setup {
        orders: vec![order("b1", 1, 98.0, 0.01, false)],
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    };
    let mut h = settled(setup());
    assert!(
        place(
            1,
            TradeWindowTicketKind::Limit,
            97.0,
            &click(&mut h, TradeWindowLadderColumn::Bid, 97.0)
        ),
        "CONTROL"
    );
    let moved = TradeWindowAction::Modify { order_client_id: "b1".to_string(), new_price: 96.0 };
    assert_eq!(
        drag(&mut h, TradeWindowLadderColumn::Buy, 98.0, 96.0),
        [moved],
        "CONTROL: a live drag reprices"
    );

    let mut h = settled(setup());
    h.state_mut().stale = true;
    h.run();
    let ls = labels(&h);
    assert!(ls.iter().any(|l| l == HINT_STALE) && !ls.iter().any(|l| l == HINT), "{ls:?}");
    let refused = [TradeWindowAction::Note {
        kind: TradeWindowStatusLineKind::Error,
        text: STALE_WHY.to_string(),
    }];
    assert_eq!(
        click(&mut h, TradeWindowLadderColumn::Bid, 97.0),
        refused,
        "a click on a stale book"
    );
    assert_eq!(h.state().state.held, None, "nothing is held for a confirm either");
    let got = drag(&mut h, TradeWindowLadderColumn::Buy, 98.0, 96.0);
    assert!(!got.iter().any(|a| matches!(a, TradeWindowAction::Modify { .. })), "a drag: {got:?}");
    assert_eq!(h.state().state.drag, None, "and no drag is left under way");
    let got = click(&mut h, TradeWindowLadderColumn::Buy, 98.0);
    assert_eq!(got, [TradeWindowAction::Cancel(vec!["b1".to_string()])], "a marker still cancels");

    // Where no own order can be shown, the stale hint offers no marker either.
    let mut h = settled(Setup { orders_why: Some("not attributed"), ..setup() });
    h.state_mut().stale = true;
    h.run();
    let ls = labels(&h);
    assert!(
        ls.iter().any(|l| l == HINT_STALE_NO_MARKERS) && !ls.iter().any(|l| l == HINT_STALE),
        "{ls:?}"
    );
}

/// [`drag_to`] with `change` made to the scene after `moves` of its four pointer moves (0: in
/// the frame after the press, before the pointer has moved at all), each move a frame of its
/// own; what the drag emitted.
fn drag_change(
    h: &mut Harness<'static, Scene>,
    from: Pos2,
    to: Pos2,
    moves: usize,
    change: impl FnOnce(&mut Scene),
) -> Vec<TradeWindowAction> {
    h.state_mut().emitted.clear();
    h.event(Event::PointerMoved(from));
    press(h, from, true);
    h.step();
    let mut change = Some(change);
    for k in 1..=4 {
        if k == moves + 1
            && let Some(c) = change.take()
        {
            c(h.state_mut());
        }
        h.event(Event::PointerMoved(from + (to - from) * (k as f32 / 4.0)));
        h.step();
    }
    if let Some(c) = change.take() {
        c(h.state_mut());
    }
    press(h, to, false);
    h.run();
    std::mem::take(&mut h.state_mut().emitted)
}

/// The FW1 review (MONEY PATH): a click on the ladder is judged against the book at the PRESS
/// as well as at the release. A press on a STALE ladder, whose rows were drawn off a frozen
/// book and stay where they were drawn until the release, places nothing even when the book is
/// live again by the release: the trader chose that row against a book that may be long gone.
/// CONTROL: live at the press and at the release, the click places; live at the press and stale
/// at the release, it is refused, as before.
#[test]
fn a_press_on_a_stale_ladder_places_nothing_even_when_the_book_is_live_at_the_release() {
    let refused = [TradeWindowAction::Note {
        kind: TradeWindowStatusLineKind::Error,
        text: STALE_WHY.to_string(),
    }];
    for (what, stale_at_press, stale_at_release, places) in [
        ("CONTROL: live throughout", false, false, true),
        ("CONTROL: stale at the release", false, true, false),
        ("stale at the press, live at the release", true, false, false),
    ] {
        let mut h = settled(Setup::default());
        h.state_mut().stale = stale_at_press;
        h.run();
        let at = cell(&h, TradeWindowLadderColumn::Bid, 97.0);
        let got = press_change_release(&mut h, at, |s| s.stale = stale_at_release);
        if places {
            assert!(place(1, TradeWindowTicketKind::Limit, 97.0, &got), "{what}: {got:?}");
        } else {
            assert_eq!(got, refused, "{what}");
            assert_eq!(h.state().state.held, None, "{what}: nothing held for a confirm");
        }
    }
}

/// The FW1 review: a drag whose book goes stale while it is under way moves nothing at the
/// drop and says why: the drop's own gate, which nothing else reaches, because the drag-start
/// gate has already let this drag begin. And a drag PRESSED on a stale ladder moves nothing
/// either, even where the book is live again before the pointer has moved far enough to start
/// it, as a click pressed there places nothing. CONTROL: the same drag on a live book reprices.
#[test]
fn a_drag_on_a_book_stale_at_its_press_or_at_its_drop_moves_nothing() {
    let setup = || Setup {
        orders: vec![order("b1", 1, 98.0, 0.01, false)],
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    };
    let refused = [TradeWindowAction::Note {
        kind: TradeWindowStatusLineKind::Error,
        text: STALE_WHY.to_string(),
    }];
    let mut h = settled(setup());
    let (from, to) = (
        cell(&h, TradeWindowLadderColumn::Buy, 98.0),
        cell(&h, TradeWindowLadderColumn::Buy, 96.0),
    );
    let moved = TradeWindowAction::Modify { order_client_id: "b1".to_string(), new_price: 96.0 };
    assert_eq!(drag_change(&mut h, from, to, 2, |_| {}), [moved], "CONTROL: a live drag");

    let mut h = settled(setup());
    let got = drag_change(&mut h, from, to, 2, |s| {
        assert!(s.state.drag.is_some(), "CONTROL: the drag is under way");
        s.stale = true;
    });
    assert_eq!(got, refused, "the book went stale while the drag was under way");

    let mut h = settled(setup());
    h.state_mut().stale = true;
    h.run();
    let got = drag_change(&mut h, from, to, 0, |s| s.stale = false);
    assert!(
        !got.iter().any(|a| matches!(a, TradeWindowAction::Modify { .. })),
        "a drag pressed on a stale ladder: {got:?}"
    );
    assert_eq!(h.state().state.drag, None, "and no drag is left under way");
}

/// The FW1 review: a drag moves the orders the marker under the press stood for, and of those
/// only the ones still working at the drop. One that filled or was cancelled while the button
/// was down is sent no `Modify` (the node refuses it, and the strip would show a rejection for
/// an order the trader no longer sees); with none left, the drop sends nothing at all.
/// CONTROL: with both still working, both move.
#[test]
fn a_drag_moves_only_the_orders_still_working_at_the_drop() {
    let setup = || Setup {
        orders: vec![order("b1", 1, 98.0, 0.01, false), order("b2", 1, 98.0, 0.02, false)],
        capabilities: vike_venues::venue_caps::BINANCE,
        ..Setup::default()
    };
    let modify = |order_client_id: &str| TradeWindowAction::Modify {
        order_client_id: order_client_id.to_string(),
        new_price: 96.0,
    };
    for (what, gone, want) in [
        ("CONTROL: both still working", vec![], vec![modify("b1"), modify("b2")]),
        ("b1 filled during the drag", vec!["b1"], vec![modify("b2")]),
        ("both went during the drag", vec!["b1", "b2"], vec![]),
    ] {
        let mut h = settled(setup());
        let (from, to) = (
            cell(&h, TradeWindowLadderColumn::Buy, 98.0),
            cell(&h, TradeWindowLadderColumn::Buy, 96.0),
        );
        let got = drag_change(&mut h, from, to, 2, |s| {
            s.orders.retain(|o| !gone.contains(&o.order_client_id.as_str()));
        });
        assert_eq!(got, want, "{what}");
    }
}

/// The FW1 review (I-1's other half): the press is judged against the markers the trader SAW,
/// the frame before it, never against the orders as they stand in the frame the press lands
/// in. An order that went in the very frame of the press was still on screen under the
/// pointer, so the press is its cancel; one that went a frame earlier was already gone from the
/// screen, so the press is on an empty cell and places.
#[test]
fn a_press_is_judged_against_the_markers_drawn_before_it() {
    let stop = || order("s1", -1, 98.0, 0.01, true);
    // The pointer comes to rest over the cell in a frame of its own, then `change`, then the
    // press goes down in the next frame, and is released later. That models the HARNESS, which
    // runs one frame per queued event (`Harness::step`), so a move and a press queued together
    // are two frames here; a real hand cannot be judged later than this, because eframe hands a
    // move and a press that arrive together to ONE pass, and egui hit-tests that pass against
    // the widgets of the pass before it.
    let rest_change_press = |h: &mut Harness<'static, Scene>, at, change: fn(&mut Scene)| {
        h.event(Event::PointerMoved(at));
        h.step();
        change(h.state_mut());
        press(h, at, true);
        h.step();
        press(h, at, false);
        h.run();
        std::mem::take(&mut h.state_mut().emitted)
    };
    let gone = |s: &mut Scene| s.orders.clear();
    let mut h = settled(Setup { orders: vec![stop()], ..Setup::default() });
    let at = cell(&h, TradeWindowLadderColumn::Sell, 98.0);
    let got = rest_change_press(&mut h, at, gone);
    assert_eq!(
        got,
        [TradeWindowAction::Cancel(vec!["s1".to_string()])],
        "gone in the press's frame"
    );

    let mut h = settled(Setup { orders: vec![stop()], ..Setup::default() });
    gone(h.state_mut());
    h.run();
    h.state_mut().emitted.clear();
    let got = rest_change_press(&mut h, at, |_| {});
    assert!(place(-1, TradeWindowTicketKind::Stop, 98.0, &got), "gone a frame before: {got:?}");
}

/// Spec §3.4's hint line sits under the ladder in the side layout (pre-flight Minor 9). Where
/// the account does not trade the symbol it says that instead, because a click there does
/// nothing. Under the ladder the compact ticket needs the height, so there is no line.
#[test]
fn the_hint_line_is_drawn_beside_the_ticket_and_says_why_a_click_does_nothing() {
    let h = settled(Setup::default());
    assert!(labels(&h).iter().any(|l| l == HINT), "{:?}", labels(&h));

    let h = settled(Setup {
        tradable: TradeWindowTradable::No { trades: &[], why: None },
        ..Setup::default()
    });
    let reason = "This account cannot trade BTCUSDT now.".to_string();
    let ls = labels(&h);
    assert!(ls.contains(&reason) && !ls.iter().any(|l| l == HINT), "{ls:?}");

    let h = settled(Setup { panel: TradeWindowTicketPlace::Under, ..Setup::default() });
    assert!(!labels(&h).iter().any(|l| l == HINT), "no hint under: {:?}", labels(&h));

    // No own order can be shown (`orders_why`): no marker to click, so none is offered.
    let h = settled(Setup { orders_why: Some("not attributed"), ..Setup::default() });
    let ls = labels(&h);
    assert!(ls.iter().any(|l| l == HINT_NO_MARKERS) && !ls.iter().any(|l| l == HINT), "{ls:?}");
}
