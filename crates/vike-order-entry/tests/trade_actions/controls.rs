//! The window's other controls: the view toggles, the cancel row, the padlock, the compact ticket,
//! the size's lot and unit.

use super::*;

/// Showing the ticket under the ladder asks for the bottom-panel window size; hiding the ladder
/// asks for the ticket's own (pre-flight I9: through real clicks, in the app's type, where the
/// plan's version ran the controls on a bare context and could only panic).
#[test]
fn the_view_controls_ask_for_the_new_window_size() {
    struct Controls {
        state: TradeWindowState,
        asked: Vec<Option<egui::Vec2>>,
    }
    let mut h = Harness::builder().with_size(egui::vec2(240.0, 60.0)).build_ui_state(
        |ui, c: &mut Controls| {
            if !vike_ui_theme::harness::appearance_ready(ui.ctx(), &ThemeAppearance::default()) {
                return;
            }
            ui.horizontal(|ui| c.asked.push(trade_window::view_controls(ui, &mut c.state)));
        },
        Controls { state: TradeWindowState::default(), asked: Vec::new() },
    );
    h.run();
    let asked = |h: &mut Harness<'_, Controls>| -> Vec<egui::Vec2> {
        let sizes = h.state().asked.iter().flatten().copied().collect();
        h.state_mut().asked.clear();
        sizes
    };
    assert!(asked(&mut h).is_empty(), "no click, no resize");
    h.get_by_label("Ticket under the ladder").click();
    h.run();
    assert_eq!(asked(&mut h), [trade::UNDER_SIZE]);
    assert_eq!(h.state().state.view.panel, TradeWindowTicketPlace::Under);
    assert_eq!(
        h.get_by_label("Ticket under the ladder").accesskit_node().toggled(),
        Some(Toggled::True),
        "the chosen place reads as pressed"
    );
    h.get_by_label("Hide the ladder").click();
    h.run();
    // The compact ticket alone (the panel under, nothing over it) asks for its own column: the
    // design's 320 pt, and the height its rows, the bar and the strip take in this look.
    let alone = layout::window_size(
        trade_window::TradeWindowPanes {
            chart: false,
            ladder: false,
            panel: TradeWindowTicketPlace::Under,
        },
        &h.ctx,
    );
    assert_eq!(asked(&mut h), [alone]);
    assert_eq!(alone.x, trade::COMPACT_ALONE_WIDTH);
    assert!(alone.y < trade::UNDER_SIZE.y, "no ladder over it: {alone:?}");
    assert!(!h.state().state.view.ladder);
    // Moving the ticket beside the ladder's place makes it the FULL ticket alone: its own width and
    // the height its whole form needs in this look (FW5 B1), which at Normal density is more than
    // the spec's 560.
    h.get_by_label("Ticket beside the ladder").click();
    h.run();
    let full = layout::window_size(
        trade_window::TradeWindowPanes {
            chart: false,
            ladder: false,
            panel: TradeWindowTicketPlace::Beside,
        },
        &h.ctx,
    );
    assert_eq!(asked(&mut h), [full]);
    // The ticket's width grows with the text (3b): the default look is Standard, 286 and the pad round it.
    let ticket_alone_w =
        layout::ticket_w(&vike_ui_theme::components::ThemeTokens::of(&h.ctx)) + 20.0;
    assert_eq!(full.x, ticket_alone_w);
    assert!(full.y > layout::TICKET_ONLY_SIZE.y, "the whole form needs more than {full:?}");
    assert_eq!(h.state().state.view.panel, TradeWindowTicketPlace::Beside);
    // The chart is a view of its own: shown, it widens the window by the pane.
    h.get_by_label("Show the tick chart").click();
    h.run();
    let with_chart = layout::window_size(
        trade_window::TradeWindowPanes {
            chart: true,
            ladder: false,
            panel: TradeWindowTicketPlace::Beside,
        },
        &h.ctx,
    );
    assert_eq!(asked(&mut h), [with_chart]);
    assert_eq!(with_chart.x, ticket_alone_w + trade::CHART_WIDTH);
    assert!(h.state().state.view.chart);
}

/// Cancel all, Bids and Asks each send their own cancel at once, even on a LIVE account (a cancel
/// is never held). Where there is nothing to cancel they say so; and on a node that cannot say
/// which account an order belongs to (Ruling R9) every one is disabled with THAT reason and claims
/// no count, where it used to say "No working orders" about orders it could not see (pre-flight
/// Minor 15).
#[test]
fn the_cancel_row_cancels_each_side_and_says_why_it_cannot() {
    let orders = vec![working("b1", 1, 99.0), working("s1", -1, 101.0)];
    let mut h = settled(Scene { orders, ..live() });
    for (label, act) in [
        ("Cancel all 2", TradeWindowAction::CancelAll),
        ("Bids", TradeWindowAction::CancelSide(1)),
        ("Asks", TradeWindowAction::CancelSide(-1)),
    ] {
        click(&mut h, label);
        assert_eq!(h.state().emitted, [act], "{label}");
        h.state_mut().emitted.clear();
    }
    let mut h = settled(Scene { orders_why: Some(UNATTRIBUTED), ..demo() });
    assert!(h.query_by_label("Cancel all 0").is_none(), "no count it cannot know");
    for label in ["Cancel all", "Bids", "Asks"] {
        assert!(disabled(&h, label), "{label}");
        assert!(says_on_hover(&mut h, label, UNATTRIBUTED), "{label}");
    }
    let mut h = settled(demo());
    assert!(disabled(&h, "Cancel all 0"));
    assert!(says_on_hover(&mut h, "Cancel all 0", ticket::NO_ORDERS_WHY));
}

/// The one-click padlock reads as PRESSED while one-click trading is on (the state its name
/// states), and a click turns it off; a LIVE window starts unpressed (pre-flight Minor 17).
#[test]
fn the_padlock_is_pressed_while_one_click_trading_is_on() {
    let lock = |h: &Harness<'_, Fixture>| {
        h.get_by_label_contains("One-click trading").accesskit_node().toggled()
    };
    let mut h = settled(demo());
    assert!(h.state().state.one_click);
    assert_eq!(lock(&h), Some(Toggled::True));
    h.get_by_label_contains("One-click trading").click();
    h.run();
    assert!(!h.state().state.one_click, "the click turned it off");
    assert_eq!(lock(&h), Some(Toggled::False));
    assert_eq!(lock(&settled(live())), Some(Toggled::False), "LIVE starts with it off");
}

/// The compact ticket sends at market without touching the order type the trader chose in the full
/// one (pre-flight Minor 14), and it shows the order's value (spec §3.6, Minor 16).
#[test]
fn the_compact_ticket_sends_at_market_and_keeps_the_traders_order_type() {
    let mut h = harness_at(demo(), trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
    h.run();
    set(&mut h, |s| s.order_type = TradeWindowTicketKind::Stop);
    assert_eq!(
        h.state().state.order_type,
        TradeWindowTicketKind::Stop,
        "the compact ticket left it alone"
    );
    assert!(h.query_all_by_label_contains("≈ 1.00 USDT").next().is_some(), "the order's value");
    h.get_by_label_contains("Buy 0.010 MKT").click();
    h.run();
    match places(h.state()).as_slice() {
        [
            TradeWindowAction::Place {
                side: 1,
                order_type: TradeWindowTicketKind::Market,
                price: None,
                quantity,
                ..
            },
        ] => {
            assert!((quantity - 0.01).abs() < 1e-12, "{quantity}");
        }
        other => panic!("one market buy, got {other:?}"),
    }
    h.state_mut().state.view.panel = TradeWindowTicketPlace::Beside;
    h.run();
    assert_eq!(
        h.state().state.order_type,
        TradeWindowTicketKind::Stop,
        "beside the ladder it is still Stop"
    );
}

/// A window opened before its catalog row arrived has no lot: its quick sizes are disabled and
/// say why rather than read `0`, its size is `0`, and nothing can be sent. Once the lot is known
/// the size is the default, and the order goes (W1 review fix 1b, pre-flight I18).
#[test]
fn a_window_opened_before_its_lot_is_known_takes_the_default_size_once_it_is() {
    let mut h = settled(Scene { grid: NO_LOT, ..demo() });
    set(&mut h, |s| s.price = "99.5".to_string());
    assert_eq!(h.state().state.size, "0");
    let quick: Vec<bool> = h
        .query_all_by_role_and_label(Role::Button, "—")
        .map(|n| n.accesskit_node().is_disabled())
        .collect();
    assert_eq!(quick, [true; 5], "five quick sizes, each disabled, none reading 0");
    h.get_all_by_role_and_label(Role::Button, "—").next().expect("a quick size").hover();
    h.run();
    let why = ticket::no_lot_reason("BTCUSDT");
    assert!(h.query_all_by_label(&why).next().is_some(), "a quick size says why");
    assert!(disabled(&h, "Buy — limit"), "no size, no order");
    click(&mut h, "Buy — limit");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    h.state_mut().scene.grid = BTC;
    h.run();
    assert_eq!(h.state().state.size, "0.010", "the default size, once the lot is known");
    h.state_mut().emitted.clear();
    click(&mut h, "Buy 0.010 limit");
    assert_eq!(places(h.state()).len(), 1, "{:?}", h.state().emitted);
}

/// The unit switch converts the size and back to the same size; with no price to convert at it is
/// refused, and the size field never holds the dash a quote value without a price prints.
#[test]
fn a_unit_switch_converts_the_size_and_never_writes_a_dash() {
    let mut h = settled(demo());
    click(&mut h, "Enter the size in USDT instead");
    assert_eq!(
        (h.state().state.unit, h.state().state.size.as_str()),
        (TradeWindowSizingUnit::Quote, "1.00")
    );
    click(&mut h, "Enter the size in BTC instead");
    assert_eq!(
        (h.state().state.unit, h.state().state.size.as_str()),
        (TradeWindowSizingUnit::Base, "0.010")
    );
    let mut h = settled(Scene { last: None, book: false, ..demo() });
    assert!(disabled(&h, "Enter the size in USDT instead"), "no price to convert at");
    click(&mut h, "Enter the size in USDT instead");
    assert_eq!(
        (h.state().state.unit, h.state().state.size.as_str()),
        (TradeWindowSizingUnit::Base, "0.010")
    );
}
