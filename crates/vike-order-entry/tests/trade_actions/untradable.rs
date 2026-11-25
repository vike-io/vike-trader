//! The window's own words where it does not trade (W4), the cancel buttons' sides and the mode
//! badge.

use super::*;

/// The causes the app STATES for a window that takes no order (`TradeWindowTradable::No::why`), each in the
/// words the app's glue puts on its status strip for it, and how that strip reads it (test words
/// here: this crate cannot see the glue; the glue's own tests pin that it hands the widget its
/// strip's words): a stopped core, a halted account, an account text that names no account (each
/// an ERROR there), an account the server does not run and a node that has said nothing yet (each
/// an Info line, which the window's own last order or rejection outranks).
const STATED: [(&str, &str, TradeWindowStatusLineKind); 5] = [
    (
        "a stopped core",
        "The server has stopped trading: a handler panicked.",
        TradeWindowStatusLineKind::Error,
    ),
    (
        "a halted account",
        "Trading is halted on this account: the server opens nothing, and this window sends \
         nothing.",
        TradeWindowStatusLineKind::Error,
    ),
    (
        "an account text that names no account",
        "This window's account is not one an order can name, so it sends nothing. Pick an \
         account.",
        TradeWindowStatusLineKind::Error,
    ),
    (
        "an account the server does not run",
        "Not running on the server.",
        TradeWindowStatusLineKind::Info,
    ),
    ("a node that has said nothing yet", "Waiting for the node.", TradeWindowStatusLineKind::Info),
];

/// An error on the strip that is NOT why the window takes no order: the window's own last
/// rejection, which outranks an Info cause on the app's strip.
const UNRELATED: &str = "Rejected by the venue: insufficient margin.";

/// Hover at `at` (the pointer first rests where nothing is); whether that ADDED `words` to the
/// screen. For a region with no accessible name of its own: the ladder's rows.
fn says_on_hover_at(h: &mut Harness<'static, Fixture>, at: egui::Pos2, words: &str) -> bool {
    h.hover_at(egui::pos2(1.0, 1.0));
    h.run();
    let before = h.query_all_by_label(words).count();
    h.hover_at(at);
    h.run();
    h.query_all_by_label(words).count() > before
}

/// The F wave's reason slot (W3 review hand-off 3, W4 fix round 2's N6): a window the app makes
/// untradable for a cause it STATES says that cause, in the app's words, in every place a
/// refusal is worded — the full ticket's line, the ladder's hint and the ladder's hover, and the
/// hover of every disabled order control (Buy, Sell, Close, Reverse, TP/SL); under the ladder, the
/// compact ticket's line and Buy and Sell's hovers — the SAME text everywhere, and never "does
/// not trade". The strip carries what the app's strip would: the cause itself for an error, and
/// for an Info cause the window's own last rejection, which outranks it there — so a widget that
/// read the strip for the cause would say the rejection.
#[test]
fn each_stated_cause_is_said_in_its_own_words_everywhere_never_does_not_trade() {
    for (what, cause, kind) in STATED {
        let error = kind == TradeWindowStatusLineKind::Error;
        let status = if error { cause } else { UNRELATED };
        // A fault or a halt leaves the account's list naming the symbol; the others have no list.
        let trades = if error { vec!["BTCUSDT".to_string()] } else { Vec::new() };
        let scene = Scene {
            tradable: false,
            trades,
            why: Some(cause),
            status: Some((TradeWindowStatusLineKind::Error, status)),
            ..demo()
        };
        let mut h = settled(scene.clone());
        set(&mut h, |s| s.price = "99.5".to_string());
        let ticket = labels_in(&h, "Order ticket");
        assert!(ticket.iter().any(|l| l == cause), "{what}: the ticket's line: {ticket:?}");
        let ladder = labels_in(&h, "Ladder");
        assert!(ladder.iter().any(|l| l == cause), "{what}: the ladder's hint: {ladder:?}");
        let all: Vec<String> = [ticket, ladder].concat();
        assert!(!all.iter().any(|l| l.contains("does not trade")), "{what}: {all:?}");
        assert!(!all.iter().any(|l| l == UNRELATED), "{what}: the strip's error: {all:?}");
        for control in ["Buy 0.010 limit", "Sell 0.010 limit", "Close", "Reverse", "TP/SL"] {
            assert!(disabled(&h, control), "{what}: {control} is disabled");
            assert!(says_on_hover(&mut h, control, cause), "{what}: {control}'s hover");
        }
        let rows = region_rect(&h, "Ladder").center();
        assert!(says_on_hover_at(&mut h, rows, cause), "{what}: the ladder's hover");
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);

        let mut h = harness_at(scene, trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
        h.run();
        let compact = labels_in(&h, "Order ticket");
        assert!(compact.iter().any(|l| l == cause), "{what}: the compact line: {compact:?}");
        assert!(!compact.iter().any(|l| l.contains("does not trade")), "{what}: {compact:?}");
        for control in ["Buy 0.010 MKT", "Sell 0.010 MKT"] {
            assert!(says_on_hover(&mut h, control, cause), "{what}: {control}'s hover");
        }
    }
}

/// W4 fix round 2, N6 — the two wrong cases the reason slot ends. With no cause STATED, an
/// unrelated error on the strip (the window's own last rejection) was shown as "the cause" for an
/// EMPTY list and for a list that NAMES the symbol. Now each says the account cannot trade the
/// symbol now, on the ticket's line, the ladder's hint and Buy's hover, and the rejection stays on
/// the strip, where it belongs. POSITIVE CONTROL: an account that trades OTHER symbols says it
/// does not trade this one, with the same error on the strip — the one case that is true.
#[test]
fn an_unrelated_error_on_the_strip_is_never_said_as_the_cause() {
    let now = "This account cannot trade BTCUSDT now.".to_string();
    for (what, trades, want) in [
        ("an empty list", Vec::new(), now.clone()),
        ("a list naming the symbol", vec!["BTCUSDT".to_string()], now.clone()),
        (
            "CONTROL: a list naming other symbols",
            vec!["ETHUSDT".to_string()],
            "This account does not trade BTCUSDT. It trades ETHUSDT.".to_string(),
        ),
    ] {
        let scene = Scene {
            tradable: false,
            trades,
            status: Some((TradeWindowStatusLineKind::Error, UNRELATED)),
            ..demo()
        };
        let mut h = settled(scene);
        set(&mut h, |s| s.price = "99.5".to_string());
        let ticket = labels_in(&h, "Order ticket");
        assert!(ticket.contains(&want), "{what}: the ticket says {want:?}: {ticket:?}");
        assert!(!ticket.iter().any(|l| l == UNRELATED), "{what}: {ticket:?}");
        assert!(!ticket.iter().any(|l| l.contains("It trades BTCUSDT")), "{what}: {ticket:?}");
        let ladder = labels_in(&h, "Ladder");
        assert!(ladder.contains(&want), "{what}: the ladder's hint says it: {ladder:?}");
        assert!(says_on_hover(&mut h, "Buy 0.010 limit", &want), "{what}: Buy says it on hover");
        let strip = labels_in(&h, "Status strip");
        assert!(strip.iter().any(|l| l == UNRELATED), "{what}: the strip keeps its line");
        // F2 review minor 9, the dropped assertion: whatever the words, Buy sends nothing.
        click(&mut h, "Buy 0.010 limit");
        assert!(h.state().emitted.is_empty(), "{what}: Buy sends nothing: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: and holds nothing");
    }
}

/// W4 fix round 1, I-3: an EMPTY symbol list with no cause stated (on the app: an older node's
/// account that is not its primary market) never proves "does not trade". The ticket, the ladder
/// and every disabled button say the account cannot trade the symbol NOW, whatever Info line the
/// strip shows.
#[test]
fn an_empty_symbol_list_says_cannot_trade_now_never_does_not_trade() {
    let waiting = "Waiting for the node.";
    let scene = Scene {
        tradable: false,
        trades: Vec::new(),
        status: Some((TradeWindowStatusLineKind::Info, waiting)),
        ..demo()
    };
    let mut h = settled(scene);
    set(&mut h, |s| s.price = "99.5".to_string());
    let now = "This account cannot trade BTCUSDT now.";
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == now), "{ticket:?}");
    assert!(labels_in(&h, "Ladder").iter().any(|l| l == now), "the ladder's hint");
    assert!(labels_in(&h, "Status strip").iter().any(|l| l == waiting), "the strip's own line");
    let all: Vec<String> =
        ["Order ticket", "Ladder"].iter().flat_map(|r| labels_in(&h, r)).collect();
    assert!(!all.iter().any(|l| l.contains("does not trade")), "{all:?}");
    assert!(says_on_hover(&mut h, "Buy 0.010 limit", now), "Buy says it on hover");
}

/// W3 review hand-off 2: the compact ticket WRITES why it takes no order (spec §4.3), as the full
/// one does; under the ladder there is no hint line, so without it the reason was hover-only.
#[test]
fn an_untradable_compact_ticket_writes_why() {
    let reason = "This account does not trade BTCUSDT. It trades ETHUSDT.";
    let mut h = harness_at(
        Scene { tradable: false, ..demo() },
        trade::UNDER_SIZE,
        TradeWindowTicketPlace::Under,
    );
    h.run();
    let ticket = labels_in(&h, "Order ticket");
    assert!(ticket.iter().any(|l| l == reason), "the compact ticket writes it: {ticket:?}");
    assert!(disabled(&h, "Buy 0.010 MKT"));
    click(&mut h, "Buy 0.010 MKT");
    assert!(h.state().emitted.is_empty(), "{:?}", h.state().emitted);
    let mut h = harness_at(demo(), trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
    h.run();
    assert!(
        !labels_in(&h, "Order ticket").iter().any(|l| l == reason),
        "CONTROL: a window that trades writes no such line"
    );
}

/// W3 review minor 7: TP/SL is an order control, so a window that takes no order offers no TP/SL
/// either: disabled, with the window's reason on hover, even where TP/SL is otherwise allowed. The
/// POSITIVE CONTROL is the same window, tradable: the toggle is enabled.
#[test]
fn take_profit_stop_loss_is_offered_only_where_the_window_trades() {
    let h = settled(demo());
    assert!(!disabled(&h, "TP/SL"), "CONTROL: a window that trades offers TP/SL");
    let mut h = settled(Scene { tradable: false, ..demo() });
    assert!(disabled(&h, "TP/SL"), "an untradable window offers no TP/SL");
    let reason = "This account does not trade BTCUSDT. It trades ETHUSDT.";
    assert!(says_on_hover(&mut h, "TP/SL", reason));
}

/// The W3 fix-round re-review: the TP/SL lines PREVIEW the exits the order would carry — in the
/// full ticket while TP/SL is ticked, and in the compact toggle's hover whether it is ticked or not
/// — and, where the order itself cannot be built, say why instead. Before this the compact hover
/// with TP/SL off read "Buy: no TP/SL", which previews nothing.
#[test]
fn the_take_profit_stop_loss_lines_preview_the_exits_or_say_why_the_order_cannot_carry_them() {
    // The full ticket, ticked, with a valid order: its exits.
    let mut h = settled(demo());
    set(&mut h, |s| {
        s.price = "100".to_string();
        s.take_profit_stop_loss = true;
    });
    assert!(h.query_by_label("Buy: TP 100.5 · SL 99.7").is_some(), "a valid order's exits");
    // ...and with no size: the order's refusal, never a price it would not carry.
    set(&mut h, |s| s.size.clear());
    let why = "Buy: Enter a size of at least one lot (0.001 BTC).";
    assert!(h.query_by_label(why).is_some(), "an order that cannot be built says why");
    // The compact toggle's hover, TP/SL OFF: what it WOULD set, at the market (ask 100.0).
    let mut h = harness_at(demo(), trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
    h.run();
    assert!(!h.state().state.take_profit_stop_loss);
    h.get_by_label("TP/SL").hover();
    h.run();
    let shown = h.query_all_by_label_contains("Buy: TP 100.5 · SL 99.7").count();
    assert!(shown > 0, "the hover previews the exits a buy would carry");
    assert_eq!(h.query_all_by_label_contains("no TP/SL").count(), 0, "never 'no TP/SL'");
}

/// The W3 fix-round re-review: under the ladder, a ticked TP/SL the account refuses replaces Post
/// only and Leverage with "Turn TP/SL off to send." (the whole reason on hover; seven fixed rows),
/// and Buy MKT and Sell MKT send nothing. A Stop entry is NOT refused here — the compact ticket
/// sends at market whatever type the full one shows — so that scene is the POSITIVE CONTROL: its
/// Buy MKT sends a market order WITH both exits.
#[test]
fn the_compact_ticket_refuses_a_ticked_take_profit_stop_loss_it_cannot_send() {
    for (what, scene, tweak, why) in refused_take_profit_stop_loss() {
        let mut h = harness_at(scene, trade::UNDER_SIZE, TradeWindowTicketPlace::Under);
        h.run();
        set(&mut h, |s| {
            tweak(s);
            s.take_profit_stop_loss = true;
        });
        if why == ticket::TAKE_PROFIT_STOP_LOSS_STOP_WHY {
            assert!(h.query_by_label("Turn TP/SL off to send.").is_none(), "{what}");
            click(&mut h, "Buy 0.010 MKT");
            match places(h.state()).as_slice() {
                [
                    TradeWindowAction::Place {
                        order_type: TradeWindowTicketKind::Market,
                        exits: Some(_),
                        ..
                    },
                ] => {}
                other => panic!("{what}: one market bracket, got {other:?}"),
            }
            continue;
        }
        assert!(h.query_by_label("Turn TP/SL off to send.").is_some(), "{what}: the words");
        for button in ["Buy 0.010 MKT", "Sell 0.010 MKT"] {
            assert!(disabled(&h, button), "{what}: {button}");
            click(&mut h, button);
        }
        assert!(h.state().emitted.is_empty(), "{what}: {:?}", h.state().emitted);
        assert!(h.state().state.held.is_none(), "{what}: nothing is held either");
        let refusal = ticket::take_profit_stop_loss_refusal(why);
        assert!(says_on_hover(&mut h, "Buy 0.010 MKT", &refusal), "{what}: on the button");
    }
}

/// Ported from the DOM's accessibility tests (pre-flight I11(g)): each cancel button cancels ITS
/// OWN side, and Cancel all both. Swapping the two `CancelSide` signs pulls the wrong half of a
/// live quote book, and nothing but a click on the drawn row can see it. Orders are present on
/// both sides, so every button is enabled.
#[test]
fn each_cancel_button_routes_to_its_own_side_of_the_book() {
    let orders = vec![working("b1", 1, 99.0), working("b2", 1, 98.0), working("s1", -1, 101.0)];
    let mut h = settled(Scene { orders, ..demo() });
    for (label, act, side) in [
        ("Bids", TradeWindowAction::CancelSide(1), "'Bids' must cancel the BUY side (+1)"),
        ("Asks", TradeWindowAction::CancelSide(-1), "'Asks' must cancel the SELL side (-1)"),
        ("Cancel all 3", TradeWindowAction::CancelAll, "'Cancel all' cancels both"),
    ] {
        click(&mut h, label);
        assert_eq!(h.state().emitted, [act], "{side}");
        h.state_mut().emitted.clear();
    }
}

/// Ported from the DOM's accessibility tests (pre-flight I11(g)): the mode chip tells the trader
/// where the next click's order goes, and reads the mode it was GIVEN (spec §7, "the mode chip
/// reads the published mode") — exactly one mode word on screen, the given one, so a chip that
/// rendered two words or the wrong one fails. Inverting it would make a live account read as safe;
/// an unknown mode reads `MODE ?`, never PAPER (pre-flight C5).
#[test]
fn the_header_badge_names_the_trading_mode_it_was_given() {
    const WORDS: [&str; 4] = ["PAPER", "DEMO", "LIVE", "MODE ?"];
    for (mode, word) in [
        (Some(AccountTier::Paper), "PAPER"),
        (Some(AccountTier::Demo), "DEMO"),
        (Some(AccountTier::Live), "LIVE"),
        (None, "MODE ?"),
    ] {
        let h = settled(Scene { mode, ..demo() });
        let bar = labels_in(&h, "Instrument bar");
        let found: Vec<&String> = bar.iter().filter(|l| WORDS.contains(&l.as_str())).collect();
        assert_eq!(found, [word], "{mode:?}: the bar's mode words: {bar:?}");
    }
}
