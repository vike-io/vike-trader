//! The status strip, a backend switch, the window price, and the body's size.

use super::*;

// ---- the status strip ------------------------------------------------------------------------

/// Spec §3.8: the strip shows the window's last order and how it came out, read from the
/// snapshot by its client order id. An order from before the window looked is not news, an
/// order of another account never takes the strip, and a new instrument starts clean.
#[test]
fn the_status_strip_follows_the_newest_order_of_the_window_account() {
    let mut tv = tools::ToolView::default();
    let mut snap = two_accounts();
    snap.orders = vec![order("old", None, OrderStatus::Filled)];
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, None, "an order from before the window looked");

    snap.orders.push(order("new", None, OrderStatus::Accepted));
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, Some((StatusKind::Info, "Buy 1 limit · ACCEPTED".into())));

    snap.orders[1].status = OrderStatus::Filled;
    snap.orders.push(order("theirs", Some("SUB"), OrderStatus::Rejected));
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(
        tv.trade_status,
        Some((StatusKind::Ok, "Buy 1 limit · FILLED".into())),
        "the window's own order, not SUB's"
    );

    follow_status(&mut tv, &snap, "binance", Some("SUB"), "BTCUSDT");
    assert_eq!(tv.trade_status, None, "a new address starts clean");
}

/// A note the shell put in the strip stays until a NEW order on the address replaces it.
#[test]
fn a_note_stays_until_a_new_order_arrives() {
    let mut tv = tools::ToolView::default();
    let mut snap = two_accounts();
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    tv.trade_status = Some((StatusKind::Error, "Not sent: the account does not trade".into()));
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status.as_ref().map(|s| s.0), Some(StatusKind::Error));
    snap.orders.push(order("next", None, OrderStatus::Submitted));
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, Some((StatusKind::Info, "Buy 1 limit · SUBMITTED".into())));
}

/// Fix round 1, M-3(a): on a node that names no order's account (Ruling R9) the strip follows
/// nothing: it cannot tell the window's orders from another account's.
#[test]
fn the_status_strip_follows_nothing_on_a_node_that_does_not_attribute_orders() {
    let mut tv = tools::ToolView::default();
    let mut snap =
        snap_with(vec![block("binance", None, "", None), block("binance", Some("SUB"), "", None)]);
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    snap.orders.push(order("whose", None, OrderStatus::Accepted));
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, None);
    assert_eq!(tv.trade_status_coid, None);
}

/// The status strip follows an order exactly when the dispatcher's `owns_order` gives it to the
/// window — a DONE order included, which the markers' `window_orders` leaves out — checked
/// against an EXPLICIT table. (This was a parity test against the glue's own copy of the rule;
/// the copy is deleted and the strip calls `owns_order` itself, so a parity test would compare
/// the rule with itself.) Each fixture order is shown to the window ALONE, so every clause
/// that excludes it — another account's order, another venue's, another symbol's, an account
/// the server does not run, a venue whose blocks attribute nothing — is visible in the strip.
/// Every wrong answer is collected, so one run names every clause a change broke.
#[test]
fn the_strip_follows_the_dispatchers_attribution_live_or_done() {
    let elsewhere = |mut o: OrderView, venue: &str, symbol: &str| -> OrderView {
        o.venue = venue.into();
        o.symbol = symbol.into();
        o
    };
    let fixtures = || {
        vec![
            order("d", None, OrderStatus::Accepted),
            order("s", Some("SUB"), OrderStatus::Accepted),
            order("x", Some("OTHER"), OrderStatus::Accepted),
            elsewhere(order("v", None, OrderStatus::Accepted), "bybit", "BTCUSDT"),
            elsewhere(order("y", None, OrderStatus::Accepted), "binance", "ETHUSDT"),
            order("f", None, OrderStatus::Filled),
        ]
    };
    let node = |blocks: Vec<VenueBlock>| {
        let mut s = snap_with(blocks);
        s.portfolio.venues.push(block("bybit", None, "BTCUSDT", Some(EngineMode::Paper)));
        s
    };
    let modern = node(vec![
        block("binance", None, "BTCUSDT", Some(EngineMode::Live)),
        block("binance", Some("SUB"), "BTCUSDT", Some(EngineMode::Demo)),
    ]);
    let one_labelled = node(vec![block("binance", Some("SUB"), "", None)]);
    let unattributed =
        node(vec![block("binance", None, "", None), block("binance", Some("SUB"), "", None)]);
    let cases: [(&str, &CoreSnapshot, Option<&str>, &[&str]); 9] = [
        ("modern", &modern, None, &["d", "f"]),
        ("modern", &modern, Some("SUB"), &["s"]),
        ("modern", &modern, Some("OTHER"), &[]),
        ("one labelled", &one_labelled, None, &[]),
        ("one labelled", &one_labelled, Some("SUB"), &["d", "s", "x", "f"]),
        ("one labelled", &one_labelled, Some("OTHER"), &[]),
        ("R9", &unattributed, None, &[]),
        ("R9", &unattributed, Some("SUB"), &[]),
        ("R9", &unattributed, Some("OTHER"), &[]),
    ];

    let mut wrong = Vec::new();
    for (name, base, account, want) in cases {
        for o in fixtures() {
            let mut tv = tools::ToolView::default();
            // the window looks at its address first, so what follows is an order it saw appear
            follow_status(&mut tv, base, "binance", account, "BTCUSDT");
            let mut snap = base.clone();
            snap.orders = vec![o.clone()];
            follow_status(&mut tv, &snap, "binance", account, "BTCUSDT");
            let follows = tv.trade_status_coid.as_deref() == Some(o.client_order_id.as_str());
            if follows != want.contains(&o.client_order_id.as_str()) {
                wrong.push(format!("{name} {account:?} {} follows={follows}", o.client_order_id));
            }
        }
    }
    assert!(wrong.is_empty(), "the strip follows the wrong orders: {wrong:?}");
}

/// M-1 of the final review (slice B): a backend SWITCH forgets what the strip was following. On
/// the new node's first snapshot a window at the same address starts clean: it shows neither
/// the old backend's order line nor the new node's newest pre-existing order as its own.
///
/// ⚠ Between the two the shell publishes the EMPTY snapshot a switch leaves
/// (`CoreSnapshot::empty("observing", "")`, `backend_conn`'s `switch_backend`), and the window
/// draws it, every frame, until the new node speaks (the FW2 review). A look taken there saw
/// no order, so the new node's newest pre-existing order then read as one that APPEARED, and
/// the strip followed it as if this window had placed it. A snapshot that has said nothing is
/// no look: it takes no baseline. The same frames run at startup, for a restored window.
#[test]
fn a_backend_switch_starts_the_strip_clean_on_the_new_node() {
    let id = egui::Id::new("trade");
    let mut tv = tools::ToolView::default();
    let mut old = two_accounts();
    follow_status(&mut tv, &old, "binance", None, "BTCUSDT");
    old.orders.push(order("mine", None, OrderStatus::Accepted));
    follow_status(&mut tv, &old, "binance", None, "BTCUSDT");
    assert!(tv.trade_status.is_some(), "the strip follows the window's order on the old node");

    let mut views = std::collections::HashMap::from([(id, tv)]);
    crate::ui::tool_views::on_backend_switch(&mut views, &mut None);
    let mut tv = views.remove(&id).expect("the window");
    let silent = CoreSnapshot::empty("observing", "");
    follow_status(&mut tv, &silent, "binance", None, "BTCUSDT");
    follow_status(&mut tv, &silent, "binance", None, "BTCUSDT");
    let mut new = two_accounts();
    new.orders = vec![order("theirs", None, OrderStatus::Accepted)];
    follow_status(&mut tv, &new, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, None, "nothing from the old node, nothing that predates the look");
    assert_eq!(tv.trade_status_coid, None);
}

/// The bar's price and the ladder's outlined row are the book's MID on the desktop (final
/// review A, Minor 17, and the FW2 review): a snapshot reaches the desktop through
/// `observe_bridge::wire_to_core`, `WireSnapshot` carries no marks, and the bridge leaves
/// `marks` empty, so the window has no last trade price to read. Built here through the real
/// bridge, not a snapshot with marks planted in it; the bar's hover says "Mid price".
#[test]
fn the_window_price_is_the_books_mid_on_a_snapshot_from_the_node() {
    use crate::backend::observe_bridge::{observe_bridge_tests::full_wire_snapshot, wire_to_core};
    let mut book = L2Book::new(0.1);
    book.apply_snapshot(
        1,
        &[vike_model::BookLevel::new(99.9, 1.0)],
        &[vike_model::BookLevel::new(100.1, 1.0)],
    );
    let snap = wire_to_core(&full_wire_snapshot());
    assert!(snap.marks.is_empty(), "CONTROL: no mark crosses the wire");
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let last = |book: Option<&L2Book>| {
        let src =
            WindowSources { book, ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT")) };
        window_parts(&src).last
    };
    let mid = last(Some(&book)).expect("the book's mid");
    assert!((mid - 100.0).abs() < 1e-9, "{mid}");
    assert_eq!(last(None), None, "no book: a dash");
}

/// M-10 of the final review (slice B): a block whose route key IS an account label, but whose
/// label disagrees with it (dropped on the wire, or naming another account), offers no row: the
/// row's account is a ROUTING answer and the two cannot both be it. The `routed.text() != label`
/// arm of `account_rows`, which `a_block_with_a_malformed_label_offers_no_row` (a route key that
/// names no label) never reaches.
#[test]
fn a_block_whose_label_disagrees_with_its_route_key_offers_no_row() {
    let mut snap = two_accounts();
    for (route_key, label) in [("binance#TEAM", None), ("binance#DESK", Some("OTHER"))] {
        snap.portfolio.venues.push(VenueBlock {
            venue: "binance".into(),
            account: label.map(|l| AccountLabel::parse(l).expect("a test label")),
            route_key: route_key.into(),
            symbol: "BTCUSDT".into(),
            mode: Some(EngineMode::Live),
            ..Default::default()
        });
    }
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let dir = two_accounts_directory();
    for dir in [None, Some(&dir)] {
        let p = window_parts(&sources(&snap, dir, &cat, ("binance", None, "BTCUSDT")));
        let binance: Vec<_> = p
            .accounts
            .iter()
            .filter(|r| r.venue == "binance")
            .map(|r| (r.account, r.name))
            .collect();
        assert_eq!(binance, [(None, "main"), (Some("SUB"), "SUB")], "directory: {}", dir.is_some());
    }
}

/// `vike_panels::trade::layout::chrome` is THIS window's: a Trade window is sized by it
/// (`layout::window_size`, FW5 B1), and a chrome counted short opens a window whose form is
/// cut again. Drawn as the desktop draws a Trade window, through the real `show_window` and
/// `tool_title_bar`: the title bar, the body margin's frame, then the body, which
/// `crates/vike-desktop/src/tool_window.rs`'s `tool_content` opens with the density's gap (modelled
/// here: that crate is not built by this gate). At every density the rect the body is handed
/// is the window's size less that chrome.
#[test]
fn the_trade_windows_body_is_its_size_less_the_chrome_its_layout_assumes() {
    use crate::ui::workspace::{TOOL_BODY_MARGIN, WinKind, WinState, show_window, tool_title_bar};
    use vike_ui_theme::appearance::{Appearance, install};
    use vike_ui_theme::metrics::Density;
    let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0));
    for density in Density::ALL {
        let ctx = egui::Context::default();
        install(&ctx, &Appearance { density, ..Appearance::default() });
        let size = egui::vec2(600.0, 700.0);
        let rect = egui::Rect::from_min_size(egui::pos2(40.0, 30.0), size);
        let mut w = WinState::tool("trade-1", WinKind::Trade, rect);
        let mut body = egui::Rect::NOTHING;
        for _ in 0..3 {
            ctx.begin_pass(egui::RawInput { screen_rect: Some(screen), ..Default::default() });
            show_window(&ctx, &mut w, screen, |ui, bounds| {
                let _ = tool_title_bar(ui, WinKind::Trade, false);
                egui::Frame::new().inner_margin(TOOL_BODY_MARGIN).show(ui, |ui| {
                    bounds.show(ui, |ui| {
                        ui.add_space(crate::ui::workspace::title_bar::body_gap(
                            WinKind::Trade,
                            density.metrics().gap,
                        ));
                        body = ui.available_rect_before_wrap();
                    });
                });
                egui::Vec2::ZERO
            });
            ctx.end_pass().drop_without_applying_deltas();
        }
        let want = size - vike_panels::trade::layout::chrome();
        assert!(
            (body.size() - want).length() < 0.01,
            "{density:?}: a {size:?} window hands its body {:?}; the layout counts {want:?}",
            body.size()
        );
    }
}

/// Decision 0140: an order the node recorded as priced off delayed (or frozen) data says so on the
/// strip, beside its status; a real-time or unlabelled one reads exactly as before.
#[test]
fn the_status_strip_names_an_order_priced_off_delayed_data() {
    let mut tv = tools::ToolView::default();
    let mut snap = two_accounts();
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    let mut delayed = order("d", None, OrderStatus::Accepted);
    delayed.data_delay = Some(vike_model::DataDelay::DELAYED);
    snap.orders.push(delayed);
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(
        tv.trade_status,
        Some((StatusKind::Info, "Buy 1 limit · ACCEPTED · DELAYED".into()))
    );

    snap.orders[0].data_delay = Some(vike_model::DataDelay::RealTime);
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, Some((StatusKind::Info, "Buy 1 limit · ACCEPTED".into())));

    snap.orders[0].data_delay = Some(vike_model::DataDelay::Frozen);
    follow_status(&mut tv, &snap, "binance", None, "BTCUSDT");
    assert_eq!(tv.trade_status, Some((StatusKind::Info, "Buy 1 limit · ACCEPTED · FROZEN".into())));
}
