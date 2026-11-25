//! A press held while the form moves under it, the fields' focus through a frame's changes, a
//! prompt that wraps, and W4's parked flips (F2).

use super::*;

/// W4 fix round 1, I-1 (MONEY PATH): a press is released as a click on the button it went down
/// on, whatever moved in between. egui gives the click to the id the PRESS landed on, and a kit
/// button's own id is its position in its row; so when Buy and Sell stack between a press and its
/// release — a quote-sized market order's labels widen on a tick of the book — the button that
/// took the pressed one's place took its click, and sent a SELL.
///
/// The compact ticket, under the ladder: Buy and Sell share a row and the cancel row is the next
/// (owner decision of 2026-10-03, card 5 (ii)). It is sized in the quote currency, in
/// a window where "Buy … MKT" and "Sell … MKT" fit side by side with one point to spare (the narrowest
/// window that holds them side by side, found by drawing it, and a point more). The press goes down on Cancel all; the
/// market moves so that each label gains a digit and Buy and Sell stack, Sell taking the cancel
/// row's place; then the release: it cancels all, and nothing else. CONTROLS: before, Buy and Sell
/// share a row; under the press they stacked, and Cancel all moved down a row.
#[test]
fn a_press_held_while_buy_and_sell_stack_is_never_released_as_another_button() {
    let grid = Grid {
        tick_size: Some(0.01),
        lot_size: Some(0.000_001),
        min_notional: None,
        min_qty: Some(0.000_001),
    };
    let orders = vec![working("b1", 1, 99.0), working("s1", -1, 101.0)];
    let scene = Scene { grid, mid: Some(100.0), orders, ..demo() };
    let mut h = harness_at(scene, egui::vec2(600.0, 680.0), TradeWindowTicketPlace::Under);
    h.run();
    set(&mut h, |s| {
        s.unit = TradeWindowSizingUnit::Quote;
        s.size = "1000".to_string();
    });
    let row = |h: &Harness<'_, Fixture>, prefix: &str| centre_of(h, prefix, " MKT").y;
    let side_by_side = narrowest(&mut h, 680.0, |h| row(h, "Buy ") == row(h, "Sell "));
    widen(&mut h, side_by_side + 1.0, 680.0);
    assert_eq!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: side by side, with a point to spare");
    let at = centre_of(&h, "Cancel all", "");
    // A tick: the book moves to a tenth, and a thousand USDT is ten times the coins.
    let sent = press_change_release(&mut h, at, |f| f.scene.mid = Some(10.0));
    assert_ne!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: the rows flipped under the press");
    assert!(centre_of(&h, "Cancel all", "").y > at.y, "CONTROL: Cancel all moved down a row");
    assert_eq!(sent, [TradeWindowAction::CancelAll], "Cancel all's own action, and nothing else");
}

/// Press at `at`, let `change` move the window under the held button, then release there; what
/// the release sent. Every step is a frame of its own, as a trader's hand gives them.
fn press_change_release(
    h: &mut Harness<'static, Fixture>,
    at: egui::Pos2,
    change: impl FnOnce(&mut Fixture),
) -> Vec<TradeWindowAction> {
    let button = |pressed| egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::default(),
    };
    h.state_mut().emitted.clear();
    h.event(egui::Event::PointerMoved(at));
    h.event(button(true));
    h.step();
    change(h.state_mut());
    h.step();
    h.step();
    h.event(button(false));
    h.run();
    std::mem::take(&mut h.state_mut().emitted)
}

/// Where the button whose words start with `prefix` and end with `suffix` is, its centre.
fn centre_of(h: &Harness<'_, Fixture>, prefix: &str, suffix: &str) -> egui::Pos2 {
    h.get_by_label(&label_of(h, prefix, suffix)).rect().center()
}

/// Draw the window `w` points wide and `height` tall, and let it settle.
fn widen(h: &mut Harness<'static, Fixture>, w: f32, height: f32) {
    h.set_size(egui::vec2(w, height));
    h.run();
}

/// The narrowest window, in whole points (down to 120), at which `holds` is true of what the window draws, found
/// by drawing it one point narrower at a time, from 700 down, until it stops holding. The tests
/// below need a window a point either side of a row's packing threshold, and the threshold is
/// whatever the ticket's buttons measure: reading it off the drawn rows keeps these tests about the
/// keyed buttons, never about how wide a button is. Narrowing, not widening, because a row that
/// stacked keeps a margin before it goes back (`ticket::rows::send_rows`): the threshold read going down
/// is the plain one, which is where a window drawn fresh at that width starts.
fn narrowest(
    h: &mut Harness<'static, Fixture>,
    height: f32,
    mut holds: impl FnMut(&mut Harness<'static, Fixture>) -> bool,
) -> f32 {
    let mut found = None;
    for w in (120..=700).rev() {
        widen(h, w as f32, height);
        if holds(h) {
            found = Some(w);
        } else if found.is_some() {
            break;
        }
    }
    // Back to the wide window it started in: a row that stacked keeps its margin until it has been
    // drawn side by side again, and the caller's next window starts fresh, as the threshold did.
    widen(h, 700.0, height);
    found.expect("no window up to 700 points wide holds it") as f32
}

/// W4 fix round 2, N4: the REVERSE of the press above, which only a keyed SELL survives. Buy and
/// Sell stand a row each; the press goes down on Sell; the market moves so that each label loses
/// two digits and they go back side by side, the cancel row taking Sell's old row; then the
/// release. Sell is still on screen, so the release is Sell's — exactly one market sell — and
/// never the button that took its row. With Sell keyed by position, its press went to that
/// button, or to nothing at all.
#[test]
fn a_press_on_sell_held_while_buy_and_sell_unstack_sells_and_nothing_else() {
    let grid = Grid {
        tick_size: Some(0.01),
        lot_size: Some(0.000_001),
        min_notional: None,
        min_qty: Some(0.000_001),
    };
    let scene = Scene { grid, mid: Some(100.0), ..demo() };
    let mut h = harness_at(scene, egui::vec2(600.0, 680.0), TradeWindowTicketPlace::Under);
    h.run();
    set(&mut h, |s| {
        s.unit = TradeWindowSizingUnit::Quote;
        s.size = "1000".to_string();
    });
    // Side by side at mid 100 with room for the two digits the stacking's margin asks for (and
    // half a point), where mid 1 adds two digits to EACH label: the narrowest window that takes
    // the stacked pair back side by side when the labels shrink, found by drawing the mid-1 window
    // first and then the mid-100 one, a point wider at a time.
    let row = |h: &Harness<'_, Fixture>, prefix: &str| centre_of(h, prefix, " MKT").y;
    let unstacks = narrowest(&mut h, 680.0, |h| {
        h.state_mut().scene.mid = Some(1.0);
        h.run();
        h.state_mut().scene.mid = Some(100.0);
        h.run();
        row(h, "Buy ") == row(h, "Sell ")
    });
    widen(&mut h, unstacks + 0.5, 680.0);
    h.state_mut().scene.mid = Some(1.0);
    h.run();
    assert_ne!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: at mid 1 they stand a row each");
    let at = centre_of(&h, "Sell ", " MKT");
    let sent = press_change_release(&mut h, at, |f| f.scene.mid = Some(100.0));
    assert_eq!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: they went back side by side");
    match sent.as_slice() {
        [
            TradeWindowAction::Place {
                side: -1, order_type: TradeWindowTicketKind::Market, ..
            },
        ] => {}
        other => panic!("the pressed Sell sells, and nothing else is sent: {other:?}"),
    }
}

/// W4 fix round 2, N4 (the finding's FIRST example): in the full ticket a press on Cancel all,
/// held while Buy and Sell stack above it, cancels all — and never sells. The pinned rows were
/// positional: stacking put Sell's row where the cancel row had been, and Sell took its press.
/// Exactly the one action: Cancel all's own.
#[test]
fn a_press_on_cancel_all_held_while_buy_and_sell_stack_cancels_all_and_nothing_else() {
    let grid = Grid {
        tick_size: Some(0.01),
        lot_size: Some(0.000_001),
        min_notional: None,
        min_qty: Some(0.000_001),
    };
    let orders = vec![working("b1", 1, 99.0), working("s1", -1, 101.0)];
    let scene = Scene { grid, mid: Some(100.0), orders, ..demo() };
    let mut h = harness_at(scene, egui::vec2(600.0, 560.0), TradeWindowTicketPlace::Beside);
    h.state_mut().state.view.ladder = false;
    h.run();
    set(&mut h, |s| {
        s.order_type = TradeWindowTicketKind::Market;
        s.unit = TradeWindowSizingUnit::Quote;
        s.size = "1000".to_string();
    });
    let row = |h: &Harness<'_, Fixture>, prefix: &str| centre_of(h, prefix, " market").y;
    let side_by_side = narrowest(&mut h, 560.0, |h| row(h, "Buy ") == row(h, "Sell "));
    widen(&mut h, side_by_side + 1.0, 560.0);
    assert_eq!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: side by side, a point to spare");
    let at = h.get_by_label("Cancel all 2").rect().center();
    let sent = press_change_release(&mut h, at, |f| f.scene.mid = Some(10.0));
    assert_ne!(row(&h, "Buy "), row(&h, "Sell "), "CONTROL: they stacked under the press");
    assert_eq!(sent, [TradeWindowAction::CancelAll], "Cancel all's own action, and nothing else");
}

/// W4 fix round 2, N4: a press on Asks held while its OWN row re-packs — the count gains a digit (9
/// working orders become 10), `Cancel all 10` no longer leaves Asks room beside it, and Asks moves
/// to the next row with the padlock — still cancels the asks, exactly once. Asks, because it is the
/// cancel that MOVES: Cancel all leads its row in every packing, so a press on it proves nothing
/// about the keys (the first version of this test pressed it, and passed un-keyed).
#[test]
fn a_press_on_asks_held_while_the_count_gains_a_digit_cancels_the_asks() {
    let mut nine: Vec<TradeWindowLadderOrder> =
        (0..8).map(|i| working(&format!("o{i}"), 1, 99.0)).collect();
    nine.push(working("o8", -1, 101.0));
    let mut h = harness_at(
        Scene { orders: nine, ..demo() },
        egui::vec2(600.0, 560.0),
        TradeWindowTicketPlace::Beside,
    );
    h.state_mut().state.view.ladder = false;
    h.run();
    let y = |h: &Harness<'_, Fixture>, name: &str| h.get_by_label(name).rect().center().y;
    let one_row = narrowest(&mut h, 560.0, |h| y(h, "Asks") == y(h, "Cancel all 9"));
    widen(&mut h, one_row + 1.0, 560.0);
    assert_eq!(y(&h, "Asks"), y(&h, "Cancel all 9"), "CONTROL: Asks fits beside Cancel all 9");
    let at = h.get_by_label("Asks").rect().center();
    let sent = press_change_release(&mut h, at, |f| f.scene.orders.push(working("o9", 1, 99.0)));
    assert_ne!(y(&h, "Asks"), y(&h, "Cancel all 10"), "CONTROL: Asks moved under the press");
    assert_eq!(sent, [TradeWindowAction::CancelSide(-1)], "the asks' own cancel, exactly once");
}

/// The text field in the ticket whose text is exactly `text`, its centre.
fn field_reading(h: &Harness<'_, Fixture>, text: &str) -> egui::Pos2 {
    let found: Vec<egui::Pos2> = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::TextInput)
        .filter(|n| n.accesskit_node().value().as_deref() == Some(text))
        .map(|n| n.rect().center())
        .collect();
    match found.as_slice() {
        [one] => *one,
        other => panic!("one field reading {text:?}, found {other:?}"),
    }
}

/// Click the field at `at` and type `words` into it, a frame for each.
fn type_into(h: &mut Harness<'static, Fixture>, at: egui::Pos2, words: &str) {
    h.event(egui::Event::PointerMoved(at));
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::default(),
        });
    }
    h.run();
    h.event(egui::Event::Text(words.to_string()));
    h.run();
}

/// W4 fix round 2, NEW-1: a fill that opens or closes the position while the trader types a price
/// keeps the price field's focus. The form's controls took their ids from their position in it,
/// and "No position on this account" is drawn only while there is none: a fill shifted every id
/// below it, egui dropped the focus of a field whose id vanished, and the rest of the typing was
/// lost — a TRUNCATED price, which Buy then sent (its label names the size, not the price).
#[test]
fn a_fill_while_the_price_is_typed_keeps_the_fields_focus() {
    for (before, after) in [
        (Some(TradeWindowPosition { size: 0.05, average_price: 99.0, unrealized_pnl: 0.05 }), None),
        (None, Some(TradeWindowPosition { size: 0.05, average_price: 99.0, unrealized_pnl: 0.05 })),
    ] {
        let mut h = settled(Scene { position: before, ..demo() });
        let at = field_reading(&h, "");
        type_into(&mut h, at, "654");
        h.state_mut().scene.position = after;
        h.run();
        h.event(egui::Event::Text("32".to_string()));
        h.run();
        assert_eq!(h.state().state.price, "65432", "{before:?} -> {after:?}: every keystroke kept");
    }
}

/// W4 fix round 2, NEW-1: a press on a quick size held across a fill sets THAT size, not the
/// control the shifted ids put in its place (a share of the buying power, Reduce only).
#[test]
fn a_fill_while_a_quick_size_is_pressed_still_sets_that_size() {
    let mut h = settled(demo());
    assert_eq!(h.state().state.size, "0.010");
    let at = h.get_by_label("0.001").rect().center();
    let sent = press_change_release(&mut h, at, |f| f.scene.position = None);
    assert_eq!(h.state().state.size, "0.001", "the quick size pressed: {sent:?}");
    assert!(!h.state().state.reduce_only, "and nothing else was clicked");
    assert!(sent.is_empty(), "{sent:?}");
}

/// W4 fix round 2, NEW-1: the same for the OTHER line the form draws only sometimes — why the
/// window takes no order. An account that stops trading the symbol while the trader types a size
/// keeps the size field's focus.
#[test]
fn an_account_that_stops_trading_while_the_size_is_typed_keeps_the_fields_focus() {
    let mut h = settled(demo());
    set(&mut h, |s| {
        s.price = "99.5".to_string();
        s.size.clear();
    });
    let at = field_reading(&h, "");
    type_into(&mut h, at, "1");
    h.state_mut().scene.tradable = false;
    h.run();
    h.event(egui::Event::Text("2".to_string()));
    h.run();
    assert_eq!(h.state().state.size, "12", "every keystroke kept");
}

/// W4 fix round 2, N2: a mark tick that adds a digit to the P/L (+9.99 to +10.01, +99.99 to
/// +100.01, +999.99 to +1000.01) never moves the form below the position line. Over every average
/// price from one digit to twelve, so that, whatever the type's widths, some line sits at the
/// ticket's edge: a wrapping line that held the P/L moved Close and everything under it by a line.
#[test]
fn a_tick_that_adds_a_digit_to_the_pl_never_moves_the_form() {
    let mut h = settled(demo());
    let close_y = |h: &Harness<'_, Fixture>| h.get_by_label("Close").rect().center().y;
    let mut moved = Vec::new();
    for digits in 1..=12 {
        let avg_px = 10f64.powi(digits) - 1.0;
        for (before, after) in [(9.99, 10.01), (99.99, 100.01), (999.99, 1000.01)] {
            h.state_mut().scene.position = Some(TradeWindowPosition {
                size: 0.05,
                average_price: avg_px,
                unrealized_pnl: before,
            });
            h.run();
            let y = close_y(&h);
            h.state_mut().scene.position = Some(TradeWindowPosition {
                size: 0.05,
                average_price: avg_px,
                unrealized_pnl: after,
            });
            h.run();
            if close_y(&h) != y {
                moved.push(format!("avg {avg_px}: {before} -> {after}"));
            }
        }
    }
    assert!(moved.is_empty(), "a P/L tick moved the form: {moved:?}");
}

/// W4 fix round 2, N3: in the one case the held prompt wraps past the strip's two lines (an
/// eight-digit price with its exits, a 260 pt window, Large text), the pass that HOLDS the order
/// laid the strip out before it saw the click. The FRAME that holds it — the click's own frame,
/// nothing after it — shows every line of the prompt inside the strip `status_line::height` derives:
/// `draw` discards that pass and draws it again.
///
/// ⚠ Asserted after `step` (the click's own frames), never after `run`: egui draws the frame after
/// any frame with input whatever the window asks (`InputState::wants_repaint_after`), so a test
/// that let the window settle passed on the code that showed the cut prompt for one frame (the
/// first two versions of this test did, the second with egui's animations off).
#[test]
fn a_prompt_that_wraps_is_drawn_whole_in_the_frame_that_holds_the_order() {
    use vike_ui_theme::metrics::ThemeDensity;
    use vike_ui_theme::type_scale::ThemeTextSize;
    let look = ThemeAppearance {
        density: ThemeDensity::Comfortable,
        text_size: ThemeTextSize::Standard,
        ..ThemeAppearance::default()
    };
    let scene = Scene { look, mid: Some(12_345_678.9), last: Some(12_345_678.9), ..live() };
    let mut h = harness_at(scene, egui::vec2(260.0, 560.0), TradeWindowTicketPlace::Beside);
    h.run();
    set(&mut h, |s| {
        s.price = "12345678.9".to_string();
        s.take_profit_stop_loss = true;
    });
    h.get_by_label("Buy 0.010 limit").click();
    // The click's frames — the pointer moving there, the press, the release — and no other.
    h.step();
    assert!(h.state().state.held.is_some(), "CONTROL: the order is held");
    let (content, strip_h) = (h.state().content, h.state().strip_h);
    let strip =
        egui::Rect::from_min_max(egui::pos2(content.min.x, content.max.y - strip_h), content.max);
    let lines: Vec<(String, egui::Rect)> = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Label)
        .filter(|n| region_of(n).as_deref() == Some("Status strip"))
        .map(|n| (n.accesskit_node().value().unwrap_or_default(), n.rect()))
        .collect();
    assert!(lines.len() > 2, "CONTROL: the prompt wrapped past two lines: {lines:?}");
    for (line, r) in &lines {
        assert!(strip.expand(0.6).contains_rect(*r), "{line:?} at {r:?} lies outside {strip:?}");
    }
}

/// W4 fix round 2's parked minor 3: RELEASING a wrapped prompt. Place on a prompt the strip grew to
/// hold (an eight-digit price with its exits, a 260 pt window, Large text) lets go of the order, so
/// the strip shrinks back in that pass, and `draw` discards the pass and draws it again
/// (`request_discard`). The frame of the click sends the held order EXACTLY ONCE — egui hands the
/// second pass no input, so the click is not seen twice — and that frame already shows the strip
/// without the prompt (the CONTROLS: no Place left, the strip back to its height). Asserted after
/// ONE `step`, the click's own frame, never after the window settles.
#[test]
fn releasing_a_wrapped_prompt_sends_the_order_once_in_the_frame_of_the_click() {
    use vike_ui_theme::metrics::ThemeDensity;
    use vike_ui_theme::type_scale::ThemeTextSize;
    let look = ThemeAppearance {
        density: ThemeDensity::Comfortable,
        text_size: ThemeTextSize::Standard,
        ..ThemeAppearance::default()
    };
    let scene = Scene { look, mid: Some(12_345_678.9), last: Some(12_345_678.9), ..live() };
    let mut h = harness_at(scene, egui::vec2(260.0, 560.0), TradeWindowTicketPlace::Beside);
    h.run();
    set(&mut h, |s| {
        s.price = "12345678.9".to_string();
        s.take_profit_stop_loss = true;
    });
    click(&mut h, "Buy 0.010 limit");
    let held = h.state().state.held.clone().expect("CONTROL: the order is held");
    let lines = labels_in(&h, "Status strip").len();
    assert!(lines > 2, "CONTROL: the prompt wrapped past two lines: {lines}");
    let tall = region_rect(&h, "Status strip").height();
    h.state_mut().emitted.clear();
    h.get_by_label("Place").click();
    h.step();
    assert_eq!(places(h.state()), [&held], "sent exactly once: {:?}", h.state().emitted);
    assert!(h.state().state.held.is_none(), "and let go");
    assert!(h.query_by_label("Place").is_none(), "CONTROL: the click's frame has no prompt left");
    let short = region_rect(&h, "Status strip").height();
    assert!(short < tall, "CONTROL: the strip shrank in the click's own frame: {tall} -> {short}");
}

/// One flip: what it is, the scene it starts from, and the change of that scene alone.
struct Flip {
    what: &'static str,
    scene: Scene,
    change: fn(&mut Scene),
}

/// What comes and goes ABOVE the full ticket's form sections between two frames without the
/// trader's doing (W4 fix round 2's NEW-1, and its parked minor 5): a fill opens or closes the
/// position, and the account stops or starts taking orders, which adds or takes away the form's
/// untradable line.
fn flips() -> [Flip; 4] {
    [
        Flip { what: "a fill closes the position", scene: demo(), change: |s| s.position = None },
        Flip {
            what: "a fill opens one",
            scene: Scene { position: None, ..demo() },
            change: |s| s.position = Some(OPEN),
        },
        Flip { what: "the account stops trading", scene: demo(), change: |s| s.tradable = false },
        Flip {
            what: "the account trades again",
            scene: Scene { tradable: false, ..demo() },
            change: |s| s.tradable = true,
        },
    ]
}

/// `form_price`: a price being typed keeps every keystroke across every flip — the untradable line
/// appearing or going above it would have moved the field's id, and with it the focus.
#[test]
fn every_flip_while_the_price_is_typed_keeps_the_fields_focus() {
    for Flip { what, scene, change: flip } in flips() {
        let mut h = settled(scene);
        let at = field_reading(&h, "");
        type_into(&mut h, at, "654");
        flip(&mut h.state_mut().scene);
        h.run();
        h.event(egui::Event::Text("32".to_string()));
        h.run();
        assert_eq!(h.state().state.price, "65432", "{what}: every keystroke kept");
    }
}

/// `form_quick`: a press on a quick size held across every flip sets THAT size, and clicks nothing
/// else.
#[test]
fn every_flip_while_a_quick_size_is_pressed_still_sets_that_size() {
    every_flip_while_pressed_sets_the_size("0.001", Some("0.010"), "0.001", "the quick size");
}

/// `form_shares`: a press on a share of the buying power held across every flip sets THAT share:
/// 10 % of 10,000 USDT at the last price, 100, is 10 BTC.
#[test]
fn every_flip_while_a_share_is_pressed_still_sets_that_share() {
    every_flip_while_pressed_sets_the_size("10%", None, "10.000", "the share");
}

/// A press on the button labelled `label`, held across every flip, sets the size to `want` (`what_set`
/// names that button in a failure) and clicks nothing else. `before`, when given, is the size every
/// flip starts from, asserted first as a CONTROL.
fn every_flip_while_pressed_sets_the_size(
    label: &str,
    before: Option<&str>,
    want: &str,
    what_set: &str,
) {
    for Flip { what, scene, change: flip } in flips() {
        let mut h = settled(scene);
        if let Some(before) = before {
            assert_eq!(h.state().state.size, before, "{what}: CONTROL: the default size");
        }
        let at = h.get_by_label(label).rect().center();
        let sent = press_change_release(&mut h, at, |f| flip(&mut f.scene));
        assert_eq!(h.state().state.size, want, "{what}: {what_set} pressed: {sent:?}");
        assert!(!h.state().state.reduce_only, "{what}: and nothing else was clicked");
        assert!(sent.is_empty(), "{what}: {sent:?}");
    }
}

/// `form_tpsl`: the TP distance being typed keeps every keystroke across a fill, and a press on the
/// TP/SL toggle held across a fill turns TP/SL on. Held across the account STOPPING trading, the
/// press does nothing — the toggle is an order control, disabled once the window takes no order —
/// and clicks nothing else. (The TP field is not drawn in a window that takes no order, and a press
/// on the disabled toggle starts no click, so the account trading again has no case here.)
#[test]
fn a_flip_while_take_profit_stop_loss_is_set_keeps_its_field_and_its_toggle_their_meaning() {
    for Flip { what, scene, change: flip } in flips().into_iter().take(2) {
        let mut h = settled(scene);
        set(&mut h, |s| {
            s.price = "99.5".to_string();
            s.take_profit_stop_loss = true;
            s.take_profit_text.clear();
        });
        let at = field_reading(&h, "");
        type_into(&mut h, at, "1");
        flip(&mut h.state_mut().scene);
        h.run();
        h.event(egui::Event::Text("2".to_string()));
        h.run();
        assert_eq!(h.state().state.take_profit_text, "12", "{what}: every keystroke kept");
    }
    for (Flip { what, scene, change: flip }, turned_on) in
        flips().into_iter().zip([true, true, false])
    {
        let mut h = settled(scene);
        let at = h.get_by_label("TP/SL").rect().center();
        let sent = press_change_release(&mut h, at, |f| flip(&mut f.scene));
        assert_eq!(h.state().state.take_profit_stop_loss, turned_on, "{what}: {sent:?}");
        assert!(!h.state().state.reduce_only, "{what}: nothing else was clicked");
        assert!(sent.is_empty(), "{what}: {sent:?}");
    }
}

/// W4 fix round 2's parked minor 5, the P/L line's sibling (N2): the full ticket's order VALUE moves
/// with every tick of a Market order's price, and gains a digit at 1,000, 10,000, …. The window is
/// sized so the value line, as the ticket wrote it, fills the form's width to half a point; then a
/// tick takes it from 999.00 to 1,001.00. Nothing under it — the quick sizes, the shares, Reduce
/// only, TP/SL — moves: a line that wrapped on the new digit moved all of them by a line.
#[test]
fn a_tick_that_adds_a_digit_to_the_order_value_never_moves_the_form() {
    let scene = Scene { book: false, last: Some(9.99), buying_power: Some(500.0), ..demo() };
    let mut h = harness_at(
        scene,
        egui::vec2(layout::TICKET_ONLY_SIZE.x, 900.0),
        TradeWindowTicketPlace::Beside,
    );
    h.state_mut().state.view.ladder = false;
    h.run();
    set(&mut h, |s| {
        s.order_type = TradeWindowTicketKind::Market;
        s.size = "100".to_string();
    });
    let value = |h: &Harness<'_, Fixture>| -> (String, egui::Rect) {
        let found: Vec<(String, egui::Rect)> = h
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Label)
            .filter(|n| region_of(n).as_deref() == Some("Order ticket"))
            .filter_map(|n| {
                let v = n.accesskit_node().value()?;
                v.starts_with("≈ ").then(|| (v, n.rect()))
            })
            .collect();
        match found.as_slice() {
            [one] => one.clone(),
            other => panic!("one value line, found {other:?}"),
        }
    };
    let t = vike_ui_theme::components::ThemeTokens::of(&h.ctx);
    let font = t.mono(vike_ui_theme::type_scale::ThemeTextRole::Caption);
    let (before, _) = value(&h);
    let w = h
        .ctx
        .fonts_mut(|f| f.layout_no_wrap(before.clone(), font.clone(), egui::Color32::WHITE))
        .size()
        .x;
    let form = region_rect(&h, ticket::FORM_NAME).width();
    h.set_size(egui::vec2(layout::TICKET_ONLY_SIZE.x + (w + 0.5 - form), 900.0));
    h.run();
    let row = h.ctx.fonts_mut(|f| f.row_height(&font));
    let (_, line) = value(&h);
    assert!(line.height() < 1.5 * row, "CONTROL: the value line fits the form: {line:?}");
    let controls = ["0.001", "10%", "Reduce only", "TP/SL"];
    let ys = |h: &Harness<'_, Fixture>| controls.map(|c| h.get_by_label(c).rect().center().y);
    let at_rest = ys(&h);
    h.state_mut().scene.last = Some(10.01);
    h.run();
    let (after, _) = value(&h);
    assert!(after.len() > before.len(), "CONTROL: the tick added a digit: {before} -> {after}");
    let moved: Vec<String> = controls
        .iter()
        .zip(at_rest.iter().zip(ys(&h)))
        .filter(|(_, (a, b))| (**a - *b).abs() > 0.5)
        .map(|(c, (a, b))| format!("{c}: {a} -> {b}"))
        .collect();
    assert!(moved.is_empty(), "{before} -> {after} moved the form: {moved:?}");
}
