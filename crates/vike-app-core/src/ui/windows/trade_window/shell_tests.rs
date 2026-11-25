//! Tests of the Trade window's shell half.

use super::*;
use crate::ui::window::WindowKind;
use crate::ui::windows::trade_window::route_lane;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use vike_execution::{Command, CoreSnapshot, OrderIntent, VenueBlock};
use vike_model::accounts::account_keys::AccountLabel;
use vike_order_entry::cockpit::CockpitCommand;
use vike_order_entry::dispatch::OrderEntryDispatchRejectReason;
use vike_order_entry::trade_window::{
    TradeWindowOrigin, TradeWindowTicketExits, TradeWindowTicketKind,
};
use vike_tradehub_client::CommandRefusal;
use vike_utility::errors::CarriedRefusal;

fn trade_win(id: &str, venue: &str, symbol: &str) -> WindowState {
    let at = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(600.0, 560.0));
    let mut w = WindowState::tool(id, WindowKind::Trade, at);
    w.venue = venue.to_string();
    w.symbol = symbol.to_string();
    w
}

fn view(account: Option<&str>, actions: Vec<TradeWindowAction>) -> WindowView {
    WindowView {
        trade_account: account.map(str::to_string),
        trade_actions: actions,
        ..WindowView::default()
    }
}

fn buy() -> TradeWindowAction {
    TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 1.0,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    }
}

fn pick(venue: &str, symbol: &str) -> TradeWindowAction {
    TradeWindowAction::PickSymbol { venue: venue.into(), symbol: symbol.into() }
}

fn pair(venue: &str, symbol: &str) -> (String, String) {
    (venue.to_string(), symbol.to_string())
}

fn at(w: &WindowState, tv: &WindowView) -> (String, String, Option<String>) {
    (w.venue.clone(), w.symbol.clone(), tv.trade_account.clone())
}

fn here(venue: &str, symbol: &str, account: Option<&str>) -> (String, String, Option<String>) {
    (venue.to_string(), symbol.to_string(), account.map(str::to_string))
}

fn empty_directory() -> AccountDirectoryWire {
    AccountDirectoryWire { venues: vec![], accounts: vec![] }
}

// ===== the drain =====

/// Carry 6 of the A3 review: a symbol on ANOTHER venue opens on that venue's default account
/// (the account the window named is not that venue's), while a symbol on the SAME venue keeps
/// the account the trader chose.
#[test]
fn a_pick_on_another_venue_resets_the_account_and_one_on_the_same_venue_keeps_it() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let mut tv = view(Some("SUB"), vec![pick("binance", "ETHUSDT")]);
    frame.drain(&mut w, &mut tv);
    assert_eq!(at(&w, &tv), here("binance", "ETHUSDT", Some("SUB")), "same venue");
    tv.trade_actions.push(pick("okx", "BTC-USDT-SWAP"));
    frame.drain(&mut w, &mut tv);
    assert_eq!(at(&w, &tv), here("okx", "BTC-USDT-SWAP", None), "another venue");
    assert_eq!(
        tv.trade_recent,
        vec![pair("okx", "BTC-USDT-SWAP"), pair("binance", "ETHUSDT")],
        "recent picks are (venue, symbol) PAIRS, newest first"
    );
}

/// C3: a recent pick names its venue, so the same symbol on two venues is two picks; a repeat
/// moves to the front rather than appearing twice; the list keeps the newest five.
#[test]
fn recent_picks_are_venue_symbol_pairs_newest_first_without_repeats() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let mut tv = WindowView::default();
    for (v, s) in [
        ("binance", "A"),
        ("okx", "A"),
        ("binance", "B"),
        ("binance", "A"),
        ("binance", "C"),
        ("binance", "D"),
        ("binance", "E"),
    ] {
        tv.trade_actions.push(pick(v, s));
        frame.drain(&mut w, &mut tv);
    }
    let want: Vec<(String, String)> =
        [("binance", "E"), ("binance", "D"), ("binance", "C"), ("binance", "A"), ("binance", "B")]
            .iter()
            .map(|(v, s)| pair(v, s))
            .collect();
    assert_eq!(tv.trade_recent, want);
}

/// C3 (the drain's side): picking an account row moves the window to that row's venue AND to
/// the symbol the row opens there — never the old venue's native symbol on the new venue.
#[test]
fn a_picked_account_opens_its_own_venue_and_symbol() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "okx", "BTC-USDT-SWAP");
    let mut tv = view(
        None,
        vec![TradeWindowAction::PickAccount {
            venue: "binance".into(),
            account: Some("SUB".into()),
            symbol: "BTCUSDT.P".into(),
        }],
    );
    frame.drain(&mut w, &mut tv);
    assert_eq!(at(&w, &tv), here("binance", "BTCUSDT.P", Some("SUB")));
}

/// Minor 30 and carry 8: every order intent carries the address the widget DREW — computed
/// once, before the drain — even when a pick earlier in the same frame has moved the window.
#[test]
fn every_order_carries_the_address_the_window_drew_even_after_a_pick_in_the_same_frame() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let mut tv =
        view(Some("SUB"), vec![buy(), pick("bybit", "ETHUSDT"), TradeWindowAction::CancelAll]);
    frame.drain(&mut w, &mut tv);
    let drawn = TradeWindowAddress::parse("binance", Some("SUB"), "BTCUSDT").expect("a label");
    assert_eq!(frame.orders, vec![(drawn.clone(), buy()), (drawn, TradeWindowAction::CancelAll)]);
    assert_eq!(at(&w, &tv), here("bybit", "ETHUSDT", None));
}

/// Carry 1 of the A3 review: a window whose account text is not an account label sends
/// NOTHING, and its strip says why. The `.ok()` habit would have read the text as the default
/// account and placed the order on a different book.
#[test]
fn a_window_whose_account_is_not_a_label_sends_nothing_and_says_why() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let mut tv = view(Some("sub"), vec![buy(), TradeWindowAction::ClosePosition]);
    tv.trade_status_order_client_id = Some("ui-1-1".into());
    frame.drain(&mut w, &mut tv);
    assert!(frame.orders.is_empty(), "never the default account: {:?}", frame.orders);
    assert_eq!(
        tv.trade_status,
        Some((TradeWindowStatusLineKind::Error, UNADDRESSABLE_WHY.to_string()))
    );
    assert_eq!(tv.trade_status_order_client_id, None, "the refusal stays on the strip");
    assert_eq!(frame.acted, None, "a refused click does not make this the window to copy");
}

/// Carry 5: a note the widget writes stays on the strip only because the drain stops the strip
/// following an order; `follow_status` would otherwise rewrite it with that order next frame.
#[test]
fn a_note_replaces_the_followed_order_on_the_strip() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let note =
        TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Info, text: "a note".into() };
    let mut tv = view(None, vec![note]);
    tv.trade_status_order_client_id = Some("ui-1-1".into());
    frame.drain(&mut w, &mut tv);
    assert_eq!(tv.trade_status, Some((TradeWindowStatusLineKind::Info, "a note".to_string())));
    assert_eq!(tv.trade_status_order_client_id, None);
    assert!(frame.orders.is_empty());
    assert_eq!(frame.acted, None, "a note is what the window says, not what the trader did");
}

/// Carry 7: a seed opens the window whenever the glue offers one, and a view-control click
/// resizes the window to the size the new view asks for.
#[test]
fn the_seed_opens_the_window_and_a_view_change_resizes_it() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "");
    let size = egui::vec2(320.0, 680.0);
    let mut tv = WindowView {
        trade_pick: Some(TradeWindowPick {
            venue: "bybit".into(),
            account: Some("SUB".into()),
            symbol: "ETHUSDT".into(),
        }),
        trade_resize: Some(size),
        ..WindowView::default()
    };
    frame.drain(&mut w, &mut tv);
    assert_eq!(at(&w, &tv), here("bybit", "ETHUSDT", Some("SUB")));
    assert_eq!(tv.trade_pick, None);
    assert_eq!(tv.trade_resize, None);
    assert_eq!(w.size, size);
    assert_eq!(w.pending, Some(egui::Rect::from_min_size(w.screen_position, size)));
    assert_eq!(frame.acted, None, "a seed is not the trader acting");
    assert_eq!(frame.books, vec![pair("bybit", "ETHUSDT")], "the seeded book is streamed");
}

/// Minor 26: a new window copies the window the trader last ACTED in, not whichever Trade
/// window the loop happened to visit last; a frame nobody acted in leaves the choice alone.
#[test]
fn the_window_to_copy_is_the_one_the_trader_acted_in() {
    let mut frame = TradeWindowFrame::default();
    let (mut a, mut b) = (trade_win("ta", "binance", "BTCUSDT"), trade_win("tb", "okx", "ETH"));
    let (mut tva, mut tvb) = (view(Some("SUB"), vec![buy()]), view(None, vec![]));
    frame.drain(&mut a, &mut tva);
    frame.drain(&mut b, &mut tvb);
    let acted = TradeWindowPick {
        venue: "binance".into(),
        account: Some("SUB".into()),
        symbol: "BTCUSDT".into(),
    };
    assert_eq!(frame.acted, Some(acted.clone()), "not the window drawn last");
    let mut last = None;
    frame.remember(&mut last);
    assert_eq!(last, Some(acted.clone()));
    let mut quiet = TradeWindowFrame::default();
    quiet.drain(&mut b, &mut tvb);
    quiet.remember(&mut last);
    assert_eq!(last, Some(acted), "nobody acted: the last choice stands");
}

/// I-1 of the A4 review: the drain EMPTIES the window's intents. An order drained once is never
/// drained again: re-addressing the same intents every frame would send the same order once a
/// frame, each time under a fresh order client id.
#[test]
fn an_order_is_drained_once_and_never_again() {
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let mut tv = view(None, vec![buy()]);
    let mut first = TradeWindowFrame::default();
    first.drain(&mut w, &mut tv);
    assert_eq!(first.orders.len(), 1, "the order goes out once");
    assert!(tv.trade_actions.is_empty(), "the drain empties the window's intents");
    let mut second = TradeWindowFrame::default();
    second.drain(&mut w, &mut tv);
    assert!(second.orders.is_empty(), "and never again: {:?}", second.orders);
    assert!(tv.trade_actions.is_empty());
}

/// Two Trade windows draining in one frame each keep their OWN address: the second window's
/// orders never take the first window's venue, account or symbol, nor the other way round.
#[test]
fn two_windows_in_one_frame_keep_their_own_addresses() {
    let mut frame = TradeWindowFrame::default();
    let mut a = trade_win("ta", "binance", "BTCUSDT");
    let mut b = trade_win("tb", "okx", "ETH-USDT-SWAP");
    let mut tva = view(Some("SUB"), vec![buy()]);
    let mut tvb = view(None, vec![buy(), TradeWindowAction::CancelAll]);
    frame.drain(&mut a, &mut tva);
    frame.drain(&mut b, &mut tvb);
    let at_a = TradeWindowAddress::parse("binance", Some("SUB"), "BTCUSDT").unwrap();
    let at_b = TradeWindowAddress::parse("okx", None, "ETH-USDT-SWAP").unwrap();
    assert_eq!(
        frame.orders,
        vec![(at_a, buy()), (at_b.clone(), buy()), (at_b, TradeWindowAction::CancelAll)]
    );
}

/// M-10/M-7 of the A4 review: a held (confirm-pending) order names no backend, so on a backend
/// switch every Trade window forgets it — a window that was not drawn during the switch
/// (minimized, or hidden behind a maximized one) would otherwise confirm it onto the NEW
/// backend's book. The window a new one copies is forgotten too: it named the old backend's
/// instrument and account.
#[test]
fn a_backend_switch_forgets_every_held_order_and_the_window_to_copy() {
    let mut views = HashMap::new();
    for id in ["shown", "minimized", "hidden"] {
        let mut tv = view(Some("SUB"), vec![]);
        tv.trade.held = Some(buy());
        tv.trade_preview = Some(TradeWindowHeldPreview {
            asked: buy(),
            answer: TradeWindowHeldPreviewSlot::default(),
        });
        views.insert(egui::Id::unique(id), tv);
    }
    let mut last = Some(TradeWindowPick {
        venue: "binance".into(),
        account: Some("SUB".into()),
        symbol: "BTCUSDT".into(),
    });
    on_backend_switch(&mut views, &mut last);
    assert!(views.values().all(|tv| tv.trade.held.is_none()), "every window, drawn or not");
    assert!(views.values().all(|tv| tv.trade_preview.is_none()), "and the old node's answer");
    assert_eq!(last, None);
}

/// M-8 of the final review (slice B): a CLOSED Trade window forgets its held (confirm-pending)
/// order, so reopening it never shows the old order with a live Place, priced at whatever was
/// typed then. A window that is open — minimized included, it is still on the rail — keeps its
/// own.
#[test]
fn a_closed_trade_window_forgets_its_held_order() {
    let mut wins = vec![
        trade_win("open", "binance", "BTCUSDT"),
        trade_win("minimized", "binance", "BTCUSDT"),
        trade_win("closed", "binance", "BTCUSDT"),
    ];
    wins[1].minimized = true;
    wins[2].open = false;
    let mut views = HashMap::new();
    for w in &wins {
        let mut tv = view(None, vec![]);
        tv.trade.held = Some(buy());
        views.insert(w.id, tv);
    }
    forget_held_of_closed_windows(&wins, &mut views);
    assert!(views[&wins[0].id].trade.held.is_some(), "an open window keeps its prompt");
    assert!(views[&wins[1].id].trade.held.is_some(), "so does a minimized one");
    assert!(views[&wins[2].id].trade.held.is_none(), "a closed one forgets it");
}

/// The window's "Connect" button asks the shell for the Connections window.
#[test]
fn connect_asks_the_shell_for_the_connections_window() {
    let mut frame = TradeWindowFrame::default();
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    let mut tv = view(None, vec![TradeWindowAction::OpenConnections]);
    frame.drain(&mut w, &mut tv);
    assert!(frame.open_connections);
    assert!(frame.orders.is_empty());
}

/// Each window with an instrument streams ITS book, by venue and native symbol; a window the
/// node has not seeded yet streams nothing.
#[test]
fn each_window_with_an_instrument_streams_its_own_book() {
    let mut frame = TradeWindowFrame::default();
    for (v, s) in [("binance", "BTCUSDT"), ("bybit", "BTCUSDT"), ("binance", "")] {
        let (mut w, mut tv) = (trade_win("t", v, s), WindowView::default());
        frame.drain(&mut w, &mut tv);
    }
    assert_eq!(frame.books, vec![pair("binance", "BTCUSDT"), pair("bybit", "BTCUSDT")]);
}

// ===== refusals on the strip =====

fn block(route_key: &str, account: Option<&str>, symbol: &str) -> VenueBlock {
    VenueBlock {
        venue: "binance".into(),
        account: account.and_then(|a| AccountLabel::parse(a).ok()),
        route_key: route_key.into(),
        symbol: symbol.into(),
        tier: Some(vike_model::AccountTier::Paper),
        ..Default::default()
    }
}

/// Binance with two engines: the default account trading BTCUSDT and SUB trading ETHUSDT.
fn two_accounts() -> CoreSnapshot {
    let mut s = CoreSnapshot::empty("binance", "BTCUSDT");
    s.portfolio.venues =
        vec![block("binance", None, "BTCUSDT"), block("binance#SUB", Some("SUB"), "ETHUSDT")];
    s
}

fn strip(
    views: &HashMap<egui::Id, WindowView>,
    w: &WindowState,
) -> Option<(TradeWindowStatusLineKind, String)> {
    views[&w.id].trade_status.clone()
}

/// ⚠ An order held for a confirm is asked of the node ONCE, as the wire would carry it and with no client
/// order id (a dry run mints none, and the door refuses a reused one). The prompt then says what came
/// back: a note, or the refusal in the node's words; nothing before the answer or when there is nothing to
/// say. Another held order asks again, letting go forgets it, and a cancel or a closed window asks nothing.
#[test]
fn a_held_order_is_asked_of_the_node_once_and_the_prompt_says_the_answer() {
    let snapshot = two_accounts();
    let limits = OrderLimits::default();
    let w = trade_win("t1", "binance", "BTCUSDT");
    let mut views = HashMap::new();
    let mut tv = WindowView::default();
    tv.trade.held = Some(buy());
    views.insert(w.id, tv);
    let asked = Mutex::new(Vec::new());
    let ask = |command: CommandWire, slot: TradeWindowHeldPreviewSlot| {
        asked.lock().unwrap().push((command, slot))
    };
    let wins = std::slice::from_ref(&w);
    preview_held_orders(wins, &mut views, &snapshot, &limits, ask);
    preview_held_orders(wins, &mut views, &snapshot, &limits, ask);
    assert_eq!(asked.lock().unwrap().len(), 1, "once, however many frames it waits");
    let slot = {
        let asked = asked.lock().unwrap();
        match &asked[0].0 {
            CommandWire::Submit(order) => {
                assert_eq!(order.order_client_id, "", "a dry run names no order client id");
                assert_eq!(order.account.as_deref(), Some("DEFAULT"), "as the send would name it");
                assert_eq!((order.venue.as_str(), order.symbol.as_str()), ("binance", "BTCUSDT"));
            }
            other => panic!("the order itself, not {other:?}"),
        }
        Arc::clone(&asked[0].1)
    };
    let says = |views: &HashMap<egui::Id, WindowView>| {
        views[&w.id].trade_preview.as_ref().and_then(TradeWindowHeldPreview::says)
    };
    assert_eq!(says(&views), None, "no answer yet");
    let note = "qty 15 goes out as 20, the venue's step is 10";
    *slot.lock().unwrap() = Some((true, None, vec![note.to_string()]));
    assert_eq!(says(&views).as_deref(), Some(note));
    *slot.lock().unwrap() = Some((false, Some("name one of these books".into()), Vec::new()));
    assert_eq!(
        says(&views).as_deref(),
        Some("The node would refuse this: name one of these books"),
        "a node that sends no id: its bare words"
    );
    let refusal = CarriedRefusal::new(
        Some("tradehub.door.account_not_named".to_string()),
        "name one of these books".to_string(),
    );
    *slot.lock().unwrap() = Some((false, Some(refusal), Vec::new()));
    assert_eq!(
        says(&views).as_deref(),
        Some(
            "The node would refuse this: error [tradehub.door.account_not_named]: name one of these books"
        ),
        "its id once, before its words"
    );
    *slot.lock().unwrap() = Some((true, None, Vec::new()));
    assert_eq!(says(&views), None, "accepted with nothing to say");

    views.get_mut(&w.id).unwrap().trade.held = Some(TradeWindowAction::Place {
        side: -1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(101.0),
        quantity: 1.0,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    });
    preview_held_orders(wins, &mut views, &snapshot, &limits, ask);
    assert_eq!(asked.lock().unwrap().len(), 2, "another order is another question");
    assert_eq!(says(&views), None, "the last order's answer is not this one's");

    views.get_mut(&w.id).unwrap().trade.held = None;
    preview_held_orders(wins, &mut views, &snapshot, &limits, ask);
    assert!(views[&w.id].trade_preview.is_none(), "let go, forgotten");

    views.get_mut(&w.id).unwrap().trade.held = Some(TradeWindowAction::CancelAll);
    preview_held_orders(wins, &mut views, &snapshot, &limits, ask);
    assert_eq!(asked.lock().unwrap().len(), 2, "a cancel states no size");

    let mut closed = trade_win("t2", "binance", "BTCUSDT");
    closed.open = false;
    let mut tv = WindowView::default();
    tv.trade.held = Some(buy());
    views.insert(closed.id, tv);
    preview_held_orders(std::slice::from_ref(&closed), &mut views, &snapshot, &limits, ask);
    assert_eq!(asked.lock().unwrap().len(), 2, "a closed window asks nothing");
}

/// Minor 31: a refusal reaches the Trade windows at its venue, symbol AND account — not a
/// window on another account of the same symbol, not a window on another symbol. Driven through
/// the real dispatcher, so the reject carries the account the planner saw: the default
/// account's preview refusal names `DEFAULT` on this two-engine venue, and still finds the
/// default-account window.
#[test]
fn a_refusal_reaches_the_windows_of_its_venue_symbol_and_account_only() {
    let snapshot = two_accounts();
    let wins = vec![
        trade_win("ta", "binance", "BTCUSDT"),
        trade_win("tb", "binance", "BTCUSDT"),
        trade_win("tc", "binance", "ETHUSDT"),
        trade_win("td", "polymarket", "tok"),
    ];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![]));
    views.insert(wins[1].id, view(Some("SUB"), vec![]));
    views.insert(wins[2].id, view(Some("SUB"), vec![]));
    views.insert(wins[3].id, view(None, vec![]));
    views.get_mut(&wins[1].id).unwrap().trade_status_order_client_id = Some("ui-9-9".into());
    let address =
        |account: Option<&str>| TradeWindowAddress::parse("binance", account, "BTCUSDT").unwrap();
    let big = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 10.0,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ladder,
    };
    let plan = plan_dispatch(
        OrderEntryDispatchInputs {
            trade_actions: vec![(address(Some("SUB")), buy()), (address(None), big)],
            cockpit_commands: vec![CockpitCommand::Submit {
                token: "tok".into(),
                side: 1,
                price: Some(0.5),
                quantity: 4000.0,
            }],
            ..Default::default()
        },
        &OrderLimits { max_notional: 500.0, ..OrderLimits::default() },
        &snapshot,
        1,
        1,
        None,
    );
    assert_eq!(plan.rejects.len(), 3, "{:?}", plan.rejects);
    route_rejects(&wins, &mut views, &plan.rejects);
    let not_traded = format!("Not sent: {}", OrderEntryDispatchRejectReason::NotTraded);
    assert_eq!(strip(&views, &wins[1]), Some((TradeWindowStatusLineKind::Error, not_traded)));
    assert_eq!(
        views[&wins[1].id].trade_status_order_client_id, None,
        "the refusal stays on the strip"
    );
    let a = strip(&views, &wins[0]).expect("the default window's own preview refusal");
    assert!(a.0 == TradeWindowStatusLineKind::Error && a.1.starts_with("Not sent: "), "{a:?}");
    assert!(!a.1.contains("does not trade"), "SUB's refusal is not the default window's: {a:?}");
    assert_eq!(strip(&views, &wins[2]), None, "another symbol");
    assert_eq!(strip(&views, &wins[3]), None, "a cockpit refusal is not a Trade window's");
}

/// **A window drawn on the lane twin sends its orders to the twin and hears its refusals there.** A
/// perpetual at the default account is routed to `binance#PERP` when the frame draws (`route_lane`); the
/// drain addresses the window's orders with the account it drew, the dispatcher names the twin on the
/// wire, and a refusal comes back to the window — and to the window that picked the twin's menu row — and
/// to no window on the spot symbol.
#[test]
fn a_window_drawn_on_the_lane_twin_sends_to_it_and_hears_its_refusals() {
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.portfolio.venues =
        vec![block("binance", None, "BTCUSDT"), block("binance#PERP", Some("PERP"), "BTCUSDT.P")];
    // The derived twin runs at the tier the table opens it at (`AccountRowLaneTwin::tiers`), demo.
    snapshot.portfolio.venues[1].tier = Some(vike_model::AccountTier::Demo);
    let mut wins = vec![
        trade_win("routed", "binance", "BTCUSDT.P"),
        trade_win("picked", "binance", "BTCUSDT.P"),
        trade_win("spot", "binance", "BTCUSDT"),
    ];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![buy(), TradeWindowAction::ClosePosition]));
    views.insert(wins[1].id, view(Some("PERP"), vec![]));
    views.insert(wins[2].id, view(None, vec![]));
    for w in &wins {
        route_lane(views.get_mut(&w.id).expect("a view"), &snapshot, &w.venue, &w.symbol, None);
    }
    let lanes: Vec<_> = wins.iter().map(|w| views[&w.id].trade_lane.clone()).collect();
    assert_eq!(
        lanes,
        [Some("PERP".to_string()), None, None],
        "only the default account's perpetual"
    );

    let mut frame = TradeWindowFrame::default();
    let tv = views.get_mut(&wins[0].id).expect("a view");
    frame.drain(&mut wins[0], tv);
    let twin = TradeWindowAddress::parse("binance", Some("PERP"), "BTCUSDT.P").expect("a label");
    assert_eq!(
        frame.orders,
        vec![(twin.clone(), buy()), (twin, TradeWindowAction::ClosePosition)],
        "the orders are the twin's"
    );
    assert_eq!(views[&wins[0].id].trade_account, None, "the trader's pick is as it was");

    let ordered = |limits: OrderLimits| {
        let inputs =
            OrderEntryDispatchInputs { trade_actions: frame.orders.clone(), ..Default::default() };
        plan_dispatch(inputs, &limits, &snapshot, 1, 1, None)
    };
    let plan = ordered(OrderLimits::default());
    match plan.commands.as_slice() {
        [Command::Order(OrderIntent::Submit(req))] => {
            assert_eq!(req.account.as_ref().map(ToString::to_string).as_deref(), Some("PERP"));
            assert_eq!((req.venue.as_str(), req.symbol.as_str()), ("binance", "BTCUSDT.P"));
        }
        other => panic!("one submit at the twin, got {other:?} / {:?}", plan.rejects),
    }
    let plan = ordered(OrderLimits { max_notional: 50.0, ..OrderLimits::default() });
    assert_eq!(plan.rejects.len(), 1, "{:?}", plan.rejects);
    route_rejects(&wins, &mut views, &plan.rejects);
    for (w, shown) in wins.iter().zip([true, true, false]) {
        let refused = strip(&views, w).is_some_and(|(kind, text)| {
            kind == TradeWindowStatusLineKind::Error && text.starts_with("Not sent: ")
        });
        assert_eq!(refused, shown, "{}", w.symbol);
    }
}

/// The NODE's refusal of an order (the interim one: "this node's `bybit` engine trades `BTCUSDT`
/// only, not `ETHUSDT`") leaves no order row, so it is put on the strip of the windows at the
/// order's venue, symbol AND account, in the node's own words and as an error — and is not
/// swallowed into the status bar's clipped control segment alone.
#[test]
fn a_node_refusal_is_said_in_the_nodes_words_on_the_windows_at_its_address() {
    let wins = vec![
        trade_win("ta", "bybit", "ETHUSDT"),
        trade_win("tb", "bybit", "ETHUSDT"),
        trade_win("tc", "bybit", "BTCUSDT"),
    ];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![]));
    views.insert(wins[1].id, view(Some("SUB"), vec![]));
    views.insert(wins[2].id, view(None, vec![]));
    views.get_mut(&wins[0].id).unwrap().trade_status_order_client_id = Some("ui-1-1".into());
    let node = "submit: this node's `bybit` engine trades `BTCUSDT` only, not `ETHUSDT`";
    let at = TradeWindowAddress { venue: "bybit".into(), symbol: "ETHUSDT".into(), account: None };
    let refused = SentOrderWord::Refused(CommandRefusal::Text(node.to_string()));
    route_node_refusals(&wins, &mut views, &[(at, refused)]);
    assert_eq!(
        strip(&views, &wins[0]),
        Some((TradeWindowStatusLineKind::Error, format!("Not placed: {node}")))
    );
    assert_eq!(
        views[&wins[0].id].trade_status_order_client_id, None,
        "the refusal stays on the strip"
    );
    assert_eq!(strip(&views, &wins[1]), None, "another account's window");
    assert_eq!(strip(&views, &wins[2]), None, "another symbol's window");
}

/// A node that answered NOT NOW (its core busy) is said on the strip in the one wording, as an INFO line: never
/// "Not placed" in the error colour, which reads as the node's final no.
#[test]
fn a_busy_node_is_not_now_on_the_strip_never_not_placed() {
    let wins = vec![trade_win("ta", "bybit", "ETHUSDT")];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![]));
    let at = TradeWindowAddress { venue: "bybit".into(), symbol: "ETHUSDT".into(), account: None };
    let busy = CommandRefusal::Frame(vike_tradehub_client::refusals::TradehubRefusal::Busy.into());
    route_node_refusals(&wins, &mut views, &[(at, SentOrderWord::NotNow(busy.clone()))]);
    assert_eq!(strip(&views, &wins[0]), Some((TradeWindowStatusLineKind::Info, busy.to_string())));
}

/// An order whose link died after it was written is OUTCOME UNKNOWN on the strip of the window that sent
/// it, as an error in the one wording every surface shows, never silence; another symbol's window says nothing.
#[test]
fn an_unknown_outcome_is_said_on_the_windows_at_its_address() {
    let wins = vec![trade_win("ta", "bybit", "ETHUSDT"), trade_win("tc", "bybit", "BTCUSDT")];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![]));
    views.insert(wins[1].id, view(None, vec![]));
    let at = TradeWindowAddress { venue: "bybit".into(), symbol: "ETHUSDT".into(), account: None };
    let unanswered = vike_tradehub_client::UnansweredCommand::LinkDropped;
    let order = crate::orders::node_refusals::SentOrder::Submit(Box::default());
    route_node_refusals(
        &wins,
        &mut views,
        &[(at, SentOrderWord::Unanswered { unanswered, order })],
    );
    assert_eq!(
        strip(&views, &wins[0]),
        Some((TradeWindowStatusLineKind::Error, unanswered.to_string()))
    );
    assert_eq!(strip(&views, &wins[1]), None, "another symbol's window");
}

/// The node's NOTE on an order it ACCEPTED and sent (placed into a closed market) is not a refusal: it
/// is kept on the windows at the order's address to ride on that order's words (`follow_status`), and
/// it touches neither the strip's text nor its colour. Another account's or symbol's window gets none.
#[test]
fn a_node_note_is_kept_for_the_windows_at_its_address_and_is_no_error() {
    let wins = vec![
        trade_win("ta", "oanda", "EURUSD"),
        trade_win("tb", "oanda", "EURUSD"),
        trade_win("tc", "oanda", "GBPUSD"),
    ];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![]));
    views.insert(wins[1].id, view(Some("SUB"), vec![]));
    views.insert(wins[2].id, view(None, vec![]));
    let note = "EURUSD: the FX market is closed until Sunday 22:00 UTC: the venue may hold, \
                refuse or queue this order";
    let at = TradeWindowAddress { venue: "oanda".into(), symbol: "EURUSD".into(), account: None };
    let word = SentOrderWord::Noted { order_client_id: "ui-1-1".into(), text: note.to_string() };
    route_node_refusals(&wins, &mut views, &[(at, word)]);
    assert_eq!(
        views[&wins[0].id].trade_note,
        Some(("ui-1-1".to_string(), note.to_string())),
        "the window at the address keeps it"
    );
    assert_eq!(strip(&views, &wins[0]), None, "the strip's own text and colour are untouched");
    assert_eq!(views[&wins[1].id].trade_note, None, "another account's window");
    assert_eq!(views[&wins[2].id].trade_note, None, "another symbol's window");
}

/// I-1 of the final review (slice B): the dispatcher's refusal of a TP/SL bracket on an engine
/// whose lane cannot hold the stop-loss (the shipped daemon's spot binance engine) reaches the
/// window that sent it, on its OWN strip — not only the node's control segment.
#[test]
fn a_bracket_refused_for_its_engines_lane_is_said_on_the_windows_strip() {
    let mut snapshot = CoreSnapshot::empty("binance", "BTCUSDT");
    snapshot.portfolio.venues = vec![block("binance", None, "BTCUSDT")];
    let wins = vec![trade_win("ta", "binance", "BTCUSDT")];
    let mut views = HashMap::from([(wins[0].id, view(None, vec![]))]);
    let bracket = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 1.0,
        reduce_only: false,
        post_only: false,
        exits: Some(TradeWindowTicketExits { take_profit: 101.0, stop_loss: 99.0 }),
        origin: TradeWindowOrigin::Ticket,
    };
    let address = TradeWindowAddress::parse("binance", None, "BTCUSDT").expect("an address");
    let plan = plan_dispatch(
        OrderEntryDispatchInputs { trade_actions: vec![(address, bracket)], ..Default::default() },
        &OrderLimits::default(),
        &snapshot,
        1,
        1,
        None,
    );
    assert!(plan.commands.is_empty(), "{:?}", plan.commands);
    route_rejects(&wins, &mut views, &plan.rejects);
    let want = format!("Not sent: {}", OrderEntryDispatchRejectReason::BracketNeedsPerpEngine);
    assert_eq!(strip(&views, &wins[0]), Some((TradeWindowStatusLineKind::Error, want)));
}

/// A desktop with no control channel (a read-only connection, or one still connecting) sends
/// nothing, and the windows that tried say so rather than swallowing the click.
#[test]
fn orders_from_a_desktop_with_no_control_channel_say_they_were_not_sent() {
    let wins = vec![trade_win("ta", "binance", "BTCUSDT"), trade_win("tb", "binance", "BTCUSDT")];
    let mut views = HashMap::new();
    views.insert(wins[0].id, view(None, vec![]));
    views.insert(wins[1].id, view(Some("SUB"), vec![]));
    let orders = vec![(TradeWindowAddress::parse("binance", None, "BTCUSDT").unwrap(), buy())];
    refuse_unsent(&wins, &mut views, &orders);
    assert_eq!(
        strip(&views, &wins[0]),
        Some((TradeWindowStatusLineKind::Error, NOT_SENT_NO_CONTROL.into()))
    );
    assert_eq!(strip(&views, &wins[1]), None, "another account's window did not try");
}

// ===== the node's directory =====

/// What a FINISHED fetch does to the slot.
#[test]
fn a_failed_refresh_keeps_the_last_good_directory_and_a_result_for_a_left_backend_is_dropped() {
    let now = Instant::now();
    let good = empty_directory();
    let refused = || Err(HubCallError::Refused { id: None, text: "directory unavailable".into() });
    let mut slot =
        TradeWindowAccountDirectory { address: "a".into(), pending: true, ..Default::default() };

    apply_directory_result(&mut slot, "a", refused(), now);
    assert!(slot.reply.is_none(), "a first fetch that fails leaves no directory");
    assert!(!slot.pending && slot.fetched_at == Some(now), "the fetch ENDED: no per-frame retry");

    slot.pending = true;
    apply_directory_result(&mut slot, "a", Ok(good.clone()), now);
    assert!(slot.reply.is_some(), "a good reply lands");

    slot.pending = true;
    apply_directory_result(&mut slot, "a", refused(), now);
    assert!(slot.reply.is_some(), "a failed refresh KEEPS the last good reply");
    assert!(!slot.pending, "...and still ends the fetch");

    let mut left =
        TradeWindowAccountDirectory { address: "b".into(), pending: true, ..Default::default() };
    apply_directory_result(&mut left, "a", Ok(good), now);
    assert!(left.reply.is_none() && left.pending, "a reply for a backend we left is dropped");
}

/// WHEN a fetch starts: once per backend, then at most once a minute while an OPEN Trade window
/// needs the list, and never while one for the SAME backend is on the wire. That bounds the
/// retries against a node that refuses the verb.
#[test]
fn the_directory_is_fetched_per_backend_and_refreshed_only_while_a_trade_window_is_open() {
    let now = Instant::now();
    let fresh = TradeWindowAccountDirectory {
        address: "a".into(),
        fetched_at: Some(now),
        ..Default::default()
    };
    assert!(!should_fetch_directory(None, &fresh, true, now), "no backend, no fetch");
    assert!(should_fetch_directory(Some("b"), &fresh, false, now), "a new backend is asked");
    assert!(!should_fetch_directory(Some("a"), &fresh, true, now), "a fresh reply stands");
    let later = now + ACCOUNT_DIRECTORY_MAX_AGE;
    assert!(should_fetch_directory(Some("a"), &fresh, true, later), "stale + open: refresh");
    assert!(!should_fetch_directory(Some("a"), &fresh, false, later), "no window, no refresh");
    let busy = TradeWindowAccountDirectory { pending: true, ..fresh.clone() };
    assert!(!should_fetch_directory(Some("a"), &busy, true, later), "one fetch at a time");
    // M-2 of the final review (slice B): a SWITCH does not wait for the left backend's fetch,
    // which may never end; the spawn resets the slot for the new backend.
    assert!(should_fetch_directory(Some("b"), &busy, true, later), "a new backend is asked");

    // `account_directory_due` asks the same question of the shell's windows: only an OPEN Trade window
    // keeps the list fresh.
    let slot = Mutex::new(fresh);
    let mut w = trade_win("t1", "binance", "BTCUSDT");
    assert!(account_directory_due(&slot, Some("a"), std::slice::from_ref(&w), later));
    w.open = false;
    assert!(!account_directory_due(&slot, Some("a"), std::slice::from_ref(&w), later), "closed");
    let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 300.0));
    let account = WindowState::tool("acc", WindowKind::Account, rect);
    assert!(!account_directory_due(&slot, Some("a"), &[account], later), "not a Trade window");
}

/// The owner's decision of 10-03, item 5: the windows say "Account list unavailable." while the
/// ACTIVE backend's fetch has ended with no list — a node that refused the verb, or one that
/// failed — including while its once-a-minute retry is on the wire. Not before the first fetch
/// has ended (the list is coming), not once a reply has landed, and not after a failed REFRESH
/// (the last good list stands); and never for a backend the slot is not about.
#[test]
fn the_account_list_is_unavailable_exactly_while_the_active_backends_fetch_ended_with_none() {
    let now = Instant::now();
    let refused = || Err(HubCallError::Refused { id: None, text: "directory unavailable".into() });
    let slot =
        Mutex::new(TradeWindowAccountDirectory { address: "a".into(), ..Default::default() });
    assert!(!account_directory_unavailable(&slot, Some("a")), "not asked yet");
    slot.lock().unwrap().pending = true;
    assert!(!account_directory_unavailable(&slot, Some("a")), "the first fetch is on the wire");
    apply_directory_result(&mut slot.lock().unwrap(), "a", refused(), now);
    assert!(account_directory_unavailable(&slot, Some("a")), "it ended with no list");
    slot.lock().unwrap().pending = true;
    assert!(account_directory_unavailable(&slot, Some("a")), "...and stays so while it is retried");
    assert!(
        !account_directory_unavailable(&slot, Some("b")),
        "another backend's slot says nothing"
    );
    assert!(!account_directory_unavailable(&slot, None), "no backend");
    apply_directory_result(&mut slot.lock().unwrap(), "a", Ok(empty_directory()), now);
    assert!(!account_directory_unavailable(&slot, Some("a")), "a reply landed");
    apply_directory_result(&mut slot.lock().unwrap(), "a", refused(), now);
    assert!(
        !account_directory_unavailable(&slot, Some("a")),
        "a failed refresh keeps the last list"
    );
}

/// A window reads the ACTIVE backend's directory and no other: after a switch, the old
/// backend's list is not shown under the new one while the new fetch is on the wire.
#[test]
fn a_window_reads_only_the_active_backends_directory() {
    let slot = Mutex::new(TradeWindowAccountDirectory {
        address: "a".into(),
        reply: Some(Arc::new(empty_directory())),
        ..Default::default()
    });
    assert!(account_directory_reply(&slot, Some("a")).is_some());
    assert!(account_directory_reply(&slot, Some("b")).is_none());
    assert!(account_directory_reply(&slot, None).is_none());
}

/// The fetch runs OFF the calling thread: the slot is marked pending before the call returns
/// (so the next frame starts no second fetch), the reply lands when the fetch ends, the frame
/// is asked to repaint, and a fetch for a NEW backend drops the old backend's list first.
#[test]
fn a_fetch_runs_off_the_calling_thread_and_a_new_backend_starts_from_nothing() {
    let slot = Arc::new(Mutex::new(TradeWindowAccountDirectory::default()));
    let (go, wait) = std::sync::mpsc::channel::<()>();
    let (painted, repaint) = std::sync::mpsc::channel::<()>();
    let fetch = move |address: &str| {
        assert_eq!(address, "a");
        wait.recv().expect("released");
        Ok(empty_directory())
    };
    let handle = spawn_directory_fetch(Arc::clone(&slot), "a".into(), fetch, move || {
        let _ = painted.send(());
    })
    .expect("spawned");
    assert!(slot.lock().unwrap().pending, "pending before the fetch has answered");
    assert!(!should_fetch_directory(Some("a"), &slot.lock().unwrap(), true, Instant::now()));
    go.send(()).unwrap();
    handle.join().expect("the fetch thread does not panic");
    repaint.try_recv().expect("the frame is asked to repaint");
    {
        let s = slot.lock().unwrap();
        assert!(!s.pending && s.reply.is_some() && s.fetched_at.is_some());
    }
    let failing =
        |_: &str| Err(HubCallError::Refused { id: None, text: "directory unavailable".into() });
    spawn_directory_fetch(Arc::clone(&slot), "b".into(), failing, || {})
        .expect("spawned")
        .join()
        .expect("no panic on a failed fetch");
    let s = slot.lock().unwrap();
    assert_eq!(s.address, "b");
    assert!(s.reply.is_none(), "b's failure never shows a's list");
    assert!(!s.pending && s.fetched_at.is_some(), "the failed fetch ended: no retry storm");
}
