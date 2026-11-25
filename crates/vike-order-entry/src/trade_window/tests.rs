//! The Trade window's unit tests: the untradable words, the held order and what drops it, the
//! confirm words, the grouping kept per instrument; how the ladder follows the book is
//! `follow_book`'s.

use super::describe::{describe, describe_parts, untradable_reason, why_untradable};
use super::state::HELD_NO_PRICE;

use super::*;

const BTC: Grid =
    Grid { tick_size: Some(0.1), lot_size: Some(0.001), min_notional: None, min_qty: Some(0.001) };

fn inputs<'a>(
    book: &'a L2Book,
    account: Option<&'a str>,
    mode: Option<AccountTier>,
) -> TradeWindowInputs<'a> {
    TradeWindowInputs {
        venue: "binance",
        venue_label: "Binance",
        product: "",
        account,
        symbol: "BTCUSDT",
        base: "BTC",
        quote: "USDT",
        mode,
        tradable: TradeWindowTradable::Yes,
        grid: BTC,
        book,
        last: None,
        stale: false,
        source: None,
        absence: None,
        top: None,
        orders: &[],
        orders_why: None,
        position: None,
        buying_power: None,
        value: vike_model::ContractValue::default(),
        capabilities: VenueCapabilities::UNSUPPORTED,
        bracket_why: account.map(|_| ticket::TAKE_PROFIT_STOP_LOSS_ACCOUNT_WHY),
        bracket_wire: false,
        post_only_wire: false,
        matches: &[],
        recent: &[],
        accounts: &[],
        accounts_why: None,
        unconnected: &[],
        tape: &[],
        status: None,
        held_says: None,
    }
}

/// The line the ladder and the ticket both show for a symbol the account does not trade: one
/// function, so the two can never word it differently (spec §4.3). An EMPTY list proves nothing
/// about the symbol, so it says the account cannot trade it now — never "does not trade", the
/// words W4 fix round 1 (I-3) took out of every other place (final review A, minor 6).
#[test]
fn the_untradable_reason_names_what_the_account_does_trade() {
    let trades = ["ETHUSDT".to_string(), "SOLUSDT".to_string()];
    assert_eq!(
        untradable_reason("BTCUSDT", &trades),
        "This account does not trade BTCUSDT. It trades ETHUSDT, SOLUSDT."
    );
    assert_eq!(untradable_reason("BTCUSDT", &[]), "This account cannot trade BTCUSDT now.");
}

/// FW6, I4: an account that trades many symbols names the first two and counts the rest, so the
/// line holds the status strip's two lines in a narrow window rather than running on; three or
/// fewer are all named ("and 1 more" says less than the name it hides). Every place that says
/// it reads this one function ([`why_untradable`]), so they shorten it alike.
#[test]
fn a_long_list_of_traded_symbols_names_two_and_counts_the_rest() {
    let names = |n: usize| -> Vec<String> {
        ["ETH-PERPETUAL", "SOL-PERPETUAL", "XRP-PERPETUAL", "DOGE-PERPETUAL", "ADA-PERPETUAL"][..n]
            .iter()
            .map(|s| s.to_string())
            .collect()
    };
    let says = |n| untradable_reason("BTCUSDT", &names(n));
    let lead = "This account does not trade BTCUSDT. It trades";
    assert_eq!(says(5), format!("{lead} ETH-PERPETUAL, SOL-PERPETUAL and 3 more."));
    assert_eq!(says(4), format!("{lead} ETH-PERPETUAL, SOL-PERPETUAL and 2 more."));
    assert_eq!(
        says(3),
        format!("{lead} ETH-PERPETUAL, SOL-PERPETUAL, XRP-PERPETUAL."),
        "CONTROL: three are all named"
    );
}

/// W3 review hand-off 3, the pure half, as the F wave's reason slot left it: what a window
/// that takes no order says. A cause the app STATES (`TradeWindowTradable::No::why`: a stopped core, a
/// halted account, …) is said in the app's words, whatever the account's list holds. With none
/// stated, a list naming OTHER symbols and not this one says the account does not trade it —
/// the one case that is true; any other list, an empty one included, says it cannot trade it
/// NOW — never "does not trade BTCUSDT. It trades BTCUSDT.", and never "does not trade" on an
/// empty list (W4 fix round 1, I-3). The status strip is never read for the cause: an Error or
/// an Info line there changes nothing (the F wave's ruling).
#[test]
fn why_untradable_names_the_cause_and_never_contradicts_itself() {
    fn why<'a>(
        book: &'a L2Book,
        trades: &'a [String],
        stated: Option<&'a str>,
        status: Option<TradeWindowStatusLine<'a>>,
    ) -> Option<String> {
        why_untradable(&TradeWindowInputs {
            tradable: TradeWindowTradable::No { trades, why: stated },
            status,
            ..inputs(book, None, Some(AccountTier::Live))
        })
    }
    let book = L2Book::new(0.1);
    let (btc, eth) = (["BTCUSDT".to_string()], ["ETHUSDT".to_string()]);
    let stopped = "The server has stopped trading: a handler panicked.";
    let error =
        Some(TradeWindowStatusLine { kind: TradeWindowStatusLineKind::Error, text: stopped });
    let info = Some(TradeWindowStatusLine {
        kind: TradeWindowStatusLineKind::Info,
        text: "Waiting for the node.",
    });
    assert_eq!(why_untradable(&inputs(&book, None, Some(AccountTier::Live))), None, "it trades");
    // A cause the app states is said as stated, whatever the account's list holds.
    assert_eq!(why(&book, &btc, Some(stopped), error).as_deref(), Some(stopped), "a fault");
    assert_eq!(why(&book, &[], Some(stopped), None).as_deref(), Some(stopped), "empty, fault");
    assert_eq!(why(&book, &eth, Some(stopped), None).as_deref(), Some(stopped), "other list");
    let now = "This account cannot trade BTCUSDT now.";
    assert_eq!(why(&book, &btc, None, None).as_deref(), Some(now));
    assert_eq!(why(&book, &btc, None, info).as_deref(), Some(now), "an Info line is no cause");
    assert_eq!(why(&book, &eth, None, error), Some(untradable_reason("BTCUSDT", &eth)));
    assert_eq!(
        why(&book, &eth, None, None).as_deref(),
        Some("This account does not trade BTCUSDT. It trades ETHUSDT."),
        "the genuine case, naming what it trades once"
    );
    // W4 fix round 1, I-3: an EMPTY list never proves "does not trade" — on the app it means
    // the account has no block at all (not running, the first snapshot not here yet).
    assert_eq!(why(&book, &[], None, None).as_deref(), Some(now), "an empty list");
    assert_eq!(why(&book, &[], None, info).as_deref(), Some(now), "an empty list, an Info strip");
}

/// The F wave's reason slot: the widget never reads the status strip for the cause. W4 fix
/// round 2's two wrong cases — an EMPTY list, and a list that NAMES the symbol, each with an
/// UNRELATED error on the strip (the window's own last rejection, a widget note, the ladder's
/// own size refusal) — showed that error as "the cause". With no stated cause they say the
/// account cannot trade the symbol now; with one, they say THAT, never the strip's error.
#[test]
fn an_unrelated_error_on_the_strip_is_never_the_cause() {
    let book = L2Book::new(0.1);
    let btc = ["BTCUSDT".to_string()];
    let now = "This account cannot trade BTCUSDT now.";
    let not_running = "Not running on the server.";
    for unrelated in [
        "Rejected by the venue: insufficient margin.",
        "Not sent: the account's mode changed while the order waited for a confirm, so it \
         was dropped.",
        "Enter a size of at least one lot (0.001 BTC).",
    ] {
        let status =
            Some(TradeWindowStatusLine { kind: TradeWindowStatusLineKind::Error, text: unrelated });
        for (what, trades, stated, want) in [
            ("an empty list", &[][..], None, now),
            ("a list naming the symbol", &btc[..], None, now),
            ("an empty list, a stated cause", &[][..], Some(not_running), not_running),
            ("a list naming the symbol, a stated cause", &btc[..], Some(not_running), not_running),
        ] {
            let got = why_untradable(&TradeWindowInputs {
                tradable: TradeWindowTradable::No { trades, why: stated },
                status,
                ..inputs(&book, None, Some(AccountTier::Live))
            });
            assert_eq!(got.as_deref(), Some(want), "{what}, with {unrelated:?} on the strip");
        }
    }
}

/// One-click starts on for PAPER and DEMO and off for LIVE and unknown; switching a window from
/// PAPER to LIVE turns it off again and says so, once (spec §3.11).
#[test]
fn one_click_follows_the_accounts_mode_and_a_switch_to_live_says_so() {
    let book = L2Book::new(0.1);
    for (mode, on) in [
        (Some(AccountTier::Paper), true),
        (Some(AccountTier::Demo), true),
        (Some(AccountTier::Live), false),
        (None, false),
    ] {
        let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
        s.sync(&inputs(&book, None, mode), &mut acts);
        assert_eq!(s.one_click, on, "{mode:?}");
        assert!(acts.is_empty(), "{mode:?}: a window's first mode leaves no note");
    }
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&inputs(&book, None, Some(AccountTier::Paper)), &mut acts);
    s.sync(&inputs(&book, Some("MAIN"), Some(AccountTier::Live)), &mut acts);
    assert!(!s.one_click, "LIVE turns one-click off");
    assert_eq!(
        acts,
        [TradeWindowAction::Note {
            kind: TradeWindowStatusLineKind::Info,
            text: ONE_CLICK_OFF_ON_LIVE.to_string()
        }]
    );
    s.sync(&inputs(&book, Some("MAIN"), Some(AccountTier::Live)), &mut acts);
    assert_eq!(acts.len(), 1, "an unchanged mode says nothing more");
}

/// An instrument change drops a held order and a drag, so neither can reach the new instrument
/// (Review Focus 4).
#[test]
fn an_instrument_change_drops_the_held_order() {
    let book = L2Book::new(0.1);
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&inputs(&book, None, Some(AccountTier::Live)), &mut acts);
    s.held = Some(TradeWindowAction::CancelAll);
    s.drag = Some(vec!["c1".to_string()]);
    s.sync(&inputs(&book, None, Some(AccountTier::Live)), &mut acts);
    assert!(s.held.is_some(), "the same instrument keeps it");
    s.sync(&inputs(&book, Some("SUB"), Some(AccountTier::Live)), &mut acts);
    assert_eq!((s.held, s.drag), (None, None));
}

/// The ladder's Group is remembered for each instrument, keyed by `(venue, symbol)` (the
/// owner's pick B of 2026-10-05): leaving BTCUSDT at 10 for ETHUSDT starts ETHUSDT at 1, and
/// coming back finds BTCUSDT at 10 and ETHUSDT at what it was left at. The book, and so the
/// grouping, is per venue and symbol, so another ACCOUNT on the same instrument keeps it.
#[test]
fn the_group_is_remembered_for_each_instrument_and_survives_an_account_change() {
    let book = L2Book::new(0.1);
    let on = |symbol, base, account| TradeWindowInputs {
        symbol,
        base,
        ..inputs(&book, account, Some(AccountTier::Paper))
    };
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&on("BTCUSDT", "BTC", None), &mut acts);
    assert_eq!(s.group, 1, "an instrument not yet seen starts at 1");
    (s.group, s.group_chosen) = (10, true); // the trader's presses
    s.sync(&on("ETHUSDT", "ETH", None), &mut acts);
    assert_eq!(s.group, 1, "another instrument, not yet seen, starts at 1");
    (s.group, s.group_chosen) = (5, true);
    s.sync(&on("BTCUSDT", "BTC", None), &mut acts);
    assert_eq!(s.group, 10, "back on the first instrument: what it was left at");
    s.sync(&on("ETHUSDT", "ETH", None), &mut acts);
    assert_eq!(s.group, 5, "back on the second: what it was left at");
    s.sync(&on("BTCUSDT", "BTC", Some("SUB")), &mut acts);
    assert_eq!(s.group, 10, "another account on the same instrument keeps the group");
}

/// W1 review fix 1b: a window opened before its catalog row arrived holds the size `0` — no
/// lot, so no size — and once the lot is known it holds the default, the middle quick size,
/// unless the trader typed one meanwhile. A lot that was known from the start never re-seeds.
#[test]
fn the_size_is_seeded_again_when_the_lot_becomes_known() {
    let book = L2Book::new(0.1);
    let unknown = Grid { lot_size: None, min_qty: None, ..BTC };
    let on = |grid| TradeWindowInputs { grid, ..inputs(&book, None, Some(AccountTier::Demo)) };
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&on(unknown), &mut acts);
    s.sync(&on(unknown), &mut acts);
    assert_eq!(s.size, "0", "no lot, no size");
    s.sync(&on(BTC), &mut acts);
    assert_eq!(s.size, "0.010", "the default size, once the lot is known");
    s.size.clear();
    s.sync(&on(BTC), &mut acts);
    assert_eq!(s.size, "", "a size cleared under a known lot stays cleared");

    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&on(unknown), &mut acts);
    s.size = "0.5".to_string();
    s.sync(&on(BTC), &mut acts);
    assert_eq!(s.size, "0.5", "a size the trader typed is kept");
}

/// The window's grid builder: a catalog number is a grid field only while it is a positive,
/// finite number; zero (either sign), a negative, an infinity and `NaN` are all none.
#[test]
fn a_grid_value_is_a_positive_finite_number_or_none() {
    for v in [0.0, -0.0, -0.001, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert_eq!(grid_value(v), None, "{v}");
    }
    for v in [0.1, 1e-8, 5.0, f64::MIN_POSITIVE] {
        assert_eq!(grid_value(v), Some(v), "{v}");
    }
}

/// W3-E, an accepted change on garbage input: a catalog lot of `-0.001` is no lot, so the size
/// field seeds `0` as for every lot not known, where the raw negative lot printed `0.000`. The
/// window refused every size on it before, as now.
#[test]
fn a_negative_lot_seeds_the_size_as_no_lot_does() {
    let book = L2Book::new(0.1);
    let grid = Grid { lot_size: grid_value(-0.001), ..BTC };
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&TradeWindowInputs { grid, ..inputs(&book, None, Some(AccountTier::Demo)) }, &mut acts);
    assert_eq!(s.size, "0");
}

/// Fix round 1, I-1: an order held for a confirm never crosses a change of the account's mode
/// on the SAME address (a PAPER account restarted LIVE, an older node's unknown mode becoming
/// known): it is dropped, and the window says so. An address change already drops it.
#[test]
fn a_held_order_does_not_cross_a_change_of_mode() {
    let book = L2Book::new(0.1);
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&inputs(&book, None, Some(AccountTier::Paper)), &mut acts);
    s.held = Some(TradeWindowAction::CancelAll);
    s.sync(&inputs(&book, None, Some(AccountTier::Paper)), &mut acts);
    assert!(s.held.is_some(), "the same mode keeps it");
    s.sync(&inputs(&book, None, Some(AccountTier::Live)), &mut acts);
    assert_eq!(s.held, None);
    assert!(
        acts.iter().any(|a| matches!(
            a,
            TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Error, text } if text.starts_with("Not sent:")
        )),
        "{acts:?}"
    );
}

/// Fix round 1, I-3: the confirm prompt says what the held order carries, its exits or none.
#[test]
fn the_confirm_words_name_the_exits_or_say_there_are_none() {
    let place = |exits| TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits,
        origin: TradeWindowOrigin::Ticket,
    };
    let bracket = TradeWindowTicketExits { take_profit: 105.0, stop_loss: 99.0 };
    assert_eq!(describe(&place(Some(bracket)), BTC), "Buy 0.010 limit @ 100.0, TP 105.0 / SL 99.0");
    assert_eq!(describe(&place(None), BTC), "Buy 0.010 limit @ 100.0, no TP/SL");
}

/// Final review A, minor 2: the confirm words say "reduce only" when the held order is: it keeps
/// the flag it was made with, whatever the box shows by the time the trader confirms it.
#[test]
fn the_confirm_words_say_reduce_only_when_the_order_is() {
    let place = |reduce_only| TradeWindowAction::Place {
        side: -1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 0.01,
        reduce_only,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ladder,
    };
    assert_eq!(describe(&place(true), BTC), "Sell 0.010 limit @ 100.0, reduce only, no TP/SL");
    assert_eq!(describe(&place(false), BTC), "Sell 0.010 limit @ 100.0, no TP/SL", "CONTROL");
}

/// The confirm prompt says a held order is post-only, as it says reduce-only: the order that
/// waits keeps the maker-only promise it was made with, and the prompt names it before the
/// trader confirms. The same order without the flag reads as before (CONTROL).
#[test]
fn the_confirm_words_say_post_only_when_the_order_is() {
    let place = |post_only, reduce_only| TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 0.01,
        reduce_only,
        post_only,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    assert_eq!(describe(&place(true, false), BTC), "Buy 0.010 limit @ 100.0, post only, no TP/SL");
    assert_eq!(
        describe(&place(true, true), BTC),
        "Buy 0.010 limit @ 100.0, reduce only, post only, no TP/SL"
    );
    assert_eq!(describe(&place(false, false), BTC), "Buy 0.010 limit @ 100.0, no TP/SL");
}

/// Final review A, minor 8: a size on a lot that is not known prints to its own decimals, never
/// as `0` ("Buy 0 limit" for an order of 0.01).
#[test]
fn a_size_on_a_lot_not_known_is_never_printed_as_zero() {
    let place = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    let unknown = Grid { lot_size: None, ..BTC };
    assert_eq!(describe(&place, unknown), "Buy 0.01 limit @ 100.0, no TP/SL");
}

/// A Limit or Stop without a price is REFUSED by the app's dispatcher (`MissingPrice`), so the
/// confirm says it will not be sent instead of promising a market fill; a Market keeps its
/// words with or without a price (CONTROL).
#[test]
fn a_priceless_limit_or_stop_is_never_described_as_a_market_order() {
    let place = |order_type, price| TradeWindowAction::Place {
        side: 1,
        order_type,
        price,
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    let order = |a: TradeWindowAction| describe_parts(&a, BTC, None).0;
    assert_eq!(
        order(place(TradeWindowTicketKind::Limit, None)),
        "Buy 0.010 limit, needs a price: not sent"
    );
    assert_eq!(
        order(place(TradeWindowTicketKind::Stop, None)),
        "Buy 0.010 stop, needs a price: not sent"
    );
    assert_eq!(order(place(TradeWindowTicketKind::Market, None)), "Buy 0.010 at market", "CONTROL");
    assert_eq!(
        order(place(TradeWindowTicketKind::Market, Some(1.0))),
        "Buy 0.010 at market",
        "CONTROL"
    );
}

/// Final review A, minor 3: a held Close or Reverse names the side and the size it sends — the
/// side opposite the position, the position's size, twice it for a Reverse, as the app's
/// dispatcher sends them — from the position it was asked against. A position nobody gave
/// keeps the plain words.
#[test]
fn a_held_close_or_reverse_names_the_side_and_size_it_sends() {
    let words = |a: TradeWindowAction, position| {
        let (order, carries) = describe_parts(&a, BTC, position);
        assert_eq!(carries, None, "{a:?}: it carries no exits");
        order
    };
    assert_eq!(
        words(TradeWindowAction::ClosePosition, Some(0.05)),
        "Close the position: sell 0.050 at market"
    );
    assert_eq!(
        words(TradeWindowAction::Reverse, Some(0.05)),
        "Reverse the position: sell 0.100 at market"
    );
    assert_eq!(
        words(TradeWindowAction::ClosePosition, Some(-0.2)),
        "Close the position: buy 0.200 at market"
    );
    assert_eq!(
        words(TradeWindowAction::Reverse, Some(-0.2)),
        "Reverse the position: buy 0.400 at market"
    );
    assert_eq!(words(TradeWindowAction::ClosePosition, None), "Close the position at market");
}

/// The position a sync sees, with `unrealized_pnl` free to tick, and a last price, so a held Reverse has
/// the price it needs and is dropped for nothing but what a test changes.
fn holding<'a>(book: &'a L2Book, size: f64, unrealized_pnl: f64) -> TradeWindowInputs<'a> {
    TradeWindowInputs {
        position: Some(TradeWindowPosition { size, average_price: 100.0, unrealized_pnl }),
        last: Some(100.0),
        ..inputs(book, None, Some(AccountTier::Live))
    }
}

/// The one error note `acts` holds, its text.
fn one_error(acts: &[TradeWindowAction]) -> &str {
    match acts {
        [TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Error, text }] => text,
        other => panic!("one error note, got {other:?}"),
    }
}

/// Final review A, minor 3 (MONEY PATH): a held Close or Reverse is sized at the confirm from the
/// position THEN, so one held while the position changed would send another size than its
/// prompt named — a Reverse held while a resting order filled sends twice the NEW position. It
/// is dropped, with a note, the moment the position's size changes. CONTROL: a tick of the
/// market moves the P/L and not the size, and keeps it.
#[test]
fn a_held_close_or_reverse_is_dropped_when_the_position_changes() {
    let book = L2Book::new(0.1);
    // A flip to the other side at the SAME size is a change too (the FW1 review): a Close held
    // against a long would sell into the new short, a Reverse would double it.
    for (what, now) in [("the position grew under it", 0.08), ("it flipped side", -0.05)] {
        for exit in [TradeWindowAction::ClosePosition, TradeWindowAction::Reverse] {
            let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
            s.sync(&holding(&book, 0.05, 0.0), &mut acts);
            s.held = Some(exit.clone());
            s.held_position = Some(0.05);
            s.sync(&holding(&book, 0.05, 3.0), &mut acts);
            assert_eq!(s.held.as_ref(), Some(&exit), "CONTROL: a tick keeps it");
            assert!(acts.is_empty(), "{acts:?}");
            s.sync(&holding(&book, now, 3.0), &mut acts);
            assert_eq!(s.held, None, "{exit:?}: {what}");
            let text = one_error(&acts);
            assert!(text.starts_with("Not sent:") && text.contains("position"), "{text}");
        }
    }
}

/// Final review A, minor 8 (MONEY PATH): a held order is sized on the lot and priced on the
/// tick it was made under. A catalog refresh that changes either drops it, with a note, before
/// a Place can send a size off the new lot or a price off the new tick — or a prompt can read
/// "Buy 0 limit" for a lot no longer known. CONTROL: the same grid keeps it.
#[test]
fn a_held_order_does_not_cross_a_change_of_grid() {
    let book = L2Book::new(0.1);
    let on = |grid| TradeWindowInputs { grid, ..inputs(&book, None, Some(AccountTier::Live)) };
    let held = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    for (what, grid) in [
        ("a new lot", Grid { lot_size: Some(0.01), ..BTC }),
        ("a new tick", Grid { tick_size: Some(0.5), ..BTC }),
        ("a lot no longer known", Grid { lot_size: None, ..BTC }),
    ] {
        let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
        s.sync(&on(BTC), &mut acts);
        s.held = Some(held.clone());
        s.sync(&on(BTC), &mut acts);
        assert!(s.held.is_some(), "{what}: CONTROL: the same grid keeps it");
        assert!(acts.is_empty(), "{what}: {acts:?}");
        s.sync(&on(grid), &mut acts);
        assert_eq!(s.held, None, "{what}");
        assert!(one_error(&acts).starts_with("Not sent:"), "{what}");
    }
}

/// W3-E, an accepted change on garbage input: a catalog refresh that moves the tick between two
/// spellings of "none" (`0`, `-0`, `NaN`, a negative, an infinity) is no change of grid. The
/// window's grid holds both as `None` ([`grid_value`]), so a held order STAYS held, where the
/// old `f64` grid saw the bits move and dropped it with the `HELD_GRID` note. CONTROL: a tick
/// that becomes known is a change, and drops it.
#[test]
fn two_spellings_of_no_tick_are_the_same_grid() {
    let book = L2Book::new(0.1);
    let on = |tick: f64| TradeWindowInputs {
        grid: Grid { tick_size: grid_value(tick), ..BTC },
        ..inputs(&book, None, Some(AccountTier::Live))
    };
    let held = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Limit,
        price: Some(100.0),
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    let none = [0.0, -0.0, f64::NAN, -0.01, f64::INFINITY];
    for (was, now) in none.iter().flat_map(|a| none.iter().map(move |b| (*a, *b))) {
        let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
        s.sync(&on(was), &mut acts);
        s.held = Some(held.clone());
        s.sync(&on(now), &mut acts);
        assert!(s.held.is_some(), "{was} -> {now}: no tick either way is no change");
        assert!(acts.is_empty(), "{was} -> {now}: {acts:?}");
        s.sync(&on(0.1), &mut acts);
        assert_eq!(s.held, None, "{was} -> 0.1: CONTROL: a tick made known is a change");
    }
}

/// Final review A, minor 5: a window that lands on an account whose mode the node did not
/// report turns one-click off (it is treated as LIVE) and says so in its own words: it never
/// calls the account LIVE while its chip reads `MODE ?`.
#[test]
fn one_click_off_on_an_unknown_mode_never_calls_the_account_live() {
    let book = L2Book::new(0.1);
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&inputs(&book, None, Some(AccountTier::Paper)), &mut acts);
    assert!(s.one_click, "CONTROL: PAPER starts with it on");
    s.sync(&inputs(&book, None, None), &mut acts);
    assert!(!s.one_click, "an unknown mode turns it off");
    match acts.as_slice() {
        [TradeWindowAction::Note { kind: TradeWindowStatusLineKind::Info, text }] => {
            assert!(!text.contains("LIVE") && text.contains("not known"), "{text}");
        }
        other => panic!("one note, got {other:?}"),
    }
}

/// F2 review minor 4: an order dropped because the window can no longer trade says WHY, in the
/// words the window states the cause in everywhere else (`why_untradable`) — not a generic
/// "cannot trade" over a cause the app stated.
#[test]
fn a_held_order_dropped_because_the_window_cannot_trade_says_the_stated_cause() {
    let book = L2Book::new(0.1);
    let stopped = "The server has stopped trading: a handler panicked.";
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    s.sync(&inputs(&book, None, Some(AccountTier::Live)), &mut acts);
    s.held = Some(TradeWindowAction::CancelAll);
    let faulted = TradeWindowInputs {
        tradable: TradeWindowTradable::No { trades: &[], why: Some(stopped) },
        ..inputs(&book, None, Some(AccountTier::Live))
    };
    s.sync(&faulted, &mut acts);
    assert_eq!(s.held, None);
    let text = one_error(&acts);
    assert!(text.starts_with("Not sent:") && text.contains(stopped), "{text}");
}

/// The FW4 dispatch (MONEY PATH, the owner's round-2 item 10, held half): an order waiting for
/// its confirm that goes out at market is kept only while the side it trades has a price — the
/// ask for a buy, the bid for a sell, else the last price. A held Reverse trades the side that
/// closes the position (a buy for a short, a sell for a long). On a book that has only the
/// OTHER side and no last price it is dropped with a note, "Not sent: …" ending in
/// `ticket::NO_MARKET_PRICE_WHY`; CONTROLS: the side it trades keeps it, and so does a last
/// price over an empty book.
#[test]
fn a_held_reverse_or_market_order_goes_when_the_side_it_trades_has_no_price() {
    let one_sided = |bids: &[vike_marketdata::L2BookLevel],
                     asks: &[vike_marketdata::L2BookLevel]| {
        let mut b = L2Book::new(0.1);
        b.apply_snapshot(1, bids, asks);
        b
    };
    let bids = one_sided(&[vike_marketdata::L2BookLevel::new(99.9, 1.0)], &[]);
    let asks = one_sided(&[], &[vike_marketdata::L2BookLevel::new(100.0, 1.0)]);
    let empty = L2Book::new(0.1);
    let market = |side| TradeWindowAction::Place {
        side,
        order_type: TradeWindowTicketKind::Market,
        price: None,
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    // What is held, the position, the book its side prices it on, the book that has only the
    // other side.
    for (what, held, size, priced, blind) in [
        ("a Reverse of a short buys", TradeWindowAction::Reverse, -0.05, &asks, &bids),
        ("a Reverse of a long sells", TradeWindowAction::Reverse, 0.05, &bids, &asks),
        ("a market buy", market(1), 0.05, &asks, &bids),
        ("a market sell", market(-1), 0.05, &bids, &asks),
    ] {
        let at = |book, last| TradeWindowInputs {
            position: Some(TradeWindowPosition { size, average_price: 100.0, unrealized_pnl: 0.0 }),
            last,
            ..inputs(book, None, Some(AccountTier::Live))
        };
        for (case, on, kept) in [
            ("CONTROL: its side is priced", at(priced, None), true),
            ("CONTROL: a last price alone", at(&empty, Some(100.0)), true),
            ("only the other side is priced", at(blind, None), false),
        ] {
            let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
            s.sync(&on, &mut acts);
            s.held = Some(held.clone());
            s.held_position = matches!(held, TradeWindowAction::Reverse).then_some(size);
            s.sync(&on, &mut acts);
            if kept {
                assert_eq!(s.held.as_ref(), Some(&held), "{what}, {case}");
                assert!(acts.is_empty(), "{what}, {case}: {acts:?}");
            } else {
                assert_eq!(s.held, None, "{what}, {case}");
                let text = one_error(&acts);
                assert!(
                    text.starts_with("Not sent:") && text.ends_with(ticket::NO_MARKET_PRICE_WHY),
                    "{what}, {case}: {text}"
                );
            }
        }
    }
}

/// The FW4 review: where a held order's window stops trading AND the side it trades loses its
/// price in the same frame, the cause the app STATES wins. One note, which says it; never the
/// price note. `sync` checks the window before the price, and this pins that order.
#[test]
fn a_held_order_whose_window_stops_trading_as_its_price_goes_says_the_stated_cause() {
    const WHY: &str = "This account is halted on the server.";
    let empty = L2Book::new(0.1);
    let held = TradeWindowAction::Place {
        side: 1,
        order_type: TradeWindowTicketKind::Market,
        price: None,
        quantity: 0.01,
        reduce_only: false,
        post_only: false,
        exits: None,
        origin: TradeWindowOrigin::Ticket,
    };
    let (mut s, mut acts) = (TradeWindowState::default(), Vec::new());
    let trading =
        TradeWindowInputs { last: Some(100.0), ..inputs(&empty, None, Some(AccountTier::Live)) };
    s.sync(&trading, &mut acts);
    s.held = Some(held.clone());
    s.sync(&trading, &mut acts);
    assert_eq!(s.held, Some(held), "CONTROL: priced and trading, it waits");
    assert!(acts.is_empty(), "{acts:?}");
    let stopped = TradeWindowInputs {
        tradable: TradeWindowTradable::No { trades: &[], why: Some(WHY) },
        ..inputs(&empty, None, Some(AccountTier::Live))
    };
    s.sync(&stopped, &mut acts);
    assert_eq!(s.held, None, "dropped");
    let text = one_error(&acts);
    assert!(text.contains(WHY), "the stated cause: {text}");
    assert!(!text.starts_with(HELD_NO_PRICE), "not the price note: {text}");
}

#[cfg(test)]
mod follow_book;
