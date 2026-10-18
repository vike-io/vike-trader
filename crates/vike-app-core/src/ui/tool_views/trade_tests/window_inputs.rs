//! I14: the window's inputs, built by one pure function.

use super::*;

// ---- I14: the window's inputs, built by one pure function ---------------------------------

/// Spec §7's wiring list: the account picker is built from a two-account snapshot, the default
/// account first; an account the database holds but the server does not run is greyed with
/// its reason and never as "switched off"; a venue with no account is listed to connect.
#[test]
fn the_account_picker_lists_both_accounts_default_first_and_greys_one_not_running() {
    let snap = two_accounts();
    let dir = two_accounts_directory();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, Some(&dir), &cat, ("binance", None, "BTCUSDT")));
    let rows: Vec<_> = p
        .accounts
        .iter()
        .map(|r| (r.venue_label, r.account, r.name, r.symbol, r.mode, r.why_not))
        .collect();
    assert_eq!(
        rows,
        [
            ("Binance", None, "main", "BTCUSDT", Some(AccountTier::Live), None),
            ("Binance", Some("SUB"), "SUB", "BTCUSDT", Some(AccountTier::Demo), None),
            ("OKX", None, "main", "BTC-USDT", Some(AccountTier::Demo), Some(NOT_RUNNING_WHY)),
        ]
    );
    let unconnected: Vec<&str> = p.unconnected.iter().map(|u| u.venue_label).collect();
    assert_eq!(unconnected, ["bybit"], "a venue with no account, by its key while untitled");
}

/// Against a node that predates the `Directory` request the list is the snapshot's running
/// accounts, named by their keys.
#[test]
fn an_older_node_lists_the_running_accounts_by_their_keys() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "BTCUSDT")));
    let rows: Vec<_> =
        p.accounts.iter().map(|r| (r.venue_label, r.account, r.name, r.why_not)).collect();
    assert_eq!(rows, [("binance", None, "main", None), ("binance", Some("SUB"), "SUB", None)]);
    assert!(p.unconnected.is_empty());
}

/// The mode chip reads the published mode, and a block that publishes none reads as unknown —
/// never as PAPER.
#[test]
fn the_mode_chip_reads_the_published_mode_and_unknown_when_none_is_published() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let at = |account| window_parts(&sources(&snap, None, &cat, ("binance", account, "BTCUSDT")));
    assert_eq!(at(None).mode, Some(AccountTier::Live));
    assert_eq!(at(Some("SUB")).mode, Some(AccountTier::Demo));

    let older = snap_with(vec![block("binance", None, "", None)]);
    let p = window_parts(&sources(&older, None, &cat, ("binance", None, "BTCUSDT")));
    assert_eq!(p.mode, None);
}

/// The buying-power sizes read the window account's own `free_bp`; an account the server does
/// not run has none.
#[test]
fn buying_power_is_the_window_accounts_free_bp() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let bp = |venue, account| {
        window_parts(&sources(&snap, None, &cat, (venue, account, "BTCUSDT"))).buying_power
    };
    assert_eq!(bp("binance", None), Some(1500.0));
    assert_eq!(bp("binance", Some("SUB")), Some(250.0));
    assert_eq!(bp("bybit", None), None);
}

/// The window's markers are `order_dispatch::window_orders`' — live orders only, of its own
/// account — so what is marked is what Cancel all pulls. A marker carries the unfilled size.
#[test]
fn the_markers_are_the_dispatchers_window_orders() {
    let mut snap = two_accounts();
    let mut partial = order("a1", None, OrderStatus::PartiallyFilled);
    partial.filled_qty = 0.25;
    snap.orders = vec![
        partial,
        order("a2", None, OrderStatus::Filled),
        order("s1", Some("SUB"), OrderStatus::Accepted),
    ];
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    for account in [None, Some("SUB")] {
        let p = window_parts(&sources(&snap, None, &cat, ("binance", account, "BTCUSDT")));
        let addr = TradeAddress::parse("binance", account, "BTCUSDT").expect("an address");
        let shared: Vec<&str> =
            window_orders(&snap, &addr).map(|o| o.client_order_id.as_str()).collect();
        let marked: Vec<&str> = p.orders.iter().map(|o| o.client_order_id.as_str()).collect();
        assert_eq!(marked, shared, "{account:?}: one rule for the markers and the cancels");
        assert_eq!(p.orders_why, None);
    }
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    assert_eq!(p.orders[0].qty, 0.75, "a marker is the unfilled size");
}

/// Ruling R9: on an older node whose two blocks publish no mode, no order names its account, so
/// the window shows no own-order markers and says why.
#[test]
fn an_older_two_block_node_shows_no_markers_and_says_why() {
    let mut snap =
        snap_with(vec![block("binance", None, "", None), block("binance", Some("SUB"), "", None)]);
    snap.orders = vec![order("a1", None, OrderStatus::Accepted)];
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    assert!(p.orders.is_empty());
    assert_eq!(p.orders_why, Some(ORDERS_UNATTRIBUTED_WHY));
    assert_eq!(
        p.status,
        Some((StatusKind::Info, Cow::Borrowed(ORDERS_UNATTRIBUTED_WHY))),
        "the strip says it too while nothing else is on it"
    );
}

/// A TP/SL bracket reaches only a venue's single default book (Ruling R3): the window offers it
/// there and nowhere else, and says the ACCOUNT reason everywhere else.
#[test]
fn brackets_are_routable_only_on_a_venues_single_default_book() {
    let mut snap = two_accounts();
    snap.portfolio.venues.push(block("okx", None, "BTC-USDT", Some(AccountTier::Demo)));
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let why = |venue, account, symbol| {
        window_parts(&sources(&snap, None, &cat, (venue, account, symbol))).bracket_why
    };
    assert_eq!(why("okx", None, "BTC-USDT"), None, "the one default book of okx");
    let account = Some(TPSL_ACCOUNT_WHY);
    assert_eq!(why("binance", None, "BTCUSDT"), account, "binance runs a second account");
    assert_eq!(why("binance", Some("SUB"), "BTCUSDT"), account, "a labelled account");
}

/// I-1 of the final review (slice B), step 3: the glue STATES why TP/SL is off, once, before
/// the click. The shipped daemon's shape — ONE default binance engine mounted on spot `BTCUSDT`
/// — gets the LANE reason (never the account one, which would be false there); a perp engine
/// (`BTCUSDT.P`) offers TP/SL; a second account on the venue is the ACCOUNT reason, which
/// outranks the lane. An engine that does not say what it trades is not judged.
#[test]
fn the_glue_says_why_tpsl_is_off_before_the_click() {
    let at = |snap: &CoreSnapshot, symbol: &str| {
        let cat = synced(&btc_catalog(), snap, "binance", symbol, "");
        window_parts(&sources(snap, None, &cat, ("binance", None, symbol))).bracket_why
    };
    let spot = snap_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    assert_eq!(at(&spot, "BTCUSDT"), Some(TPSL_LANE_WHY), "the shipped daemon's spot engine");
    let perp = snap_with(vec![block("binance", None, "BTCUSDT.P", Some(AccountTier::Live))]);
    assert_eq!(at(&perp, "BTCUSDT.P"), None, "a perp engine takes a bracket");
    assert_eq!(at(&two_accounts(), "BTCUSDT"), Some(TPSL_ACCOUNT_WHY), "the account first");
    let older = snap_with(vec![block("binance", None, "", None)]);
    assert_eq!(at(&older, "BTCUSDT"), None, "an engine that does not say is not judged");
}

/// The shipped daemon's shape drawn through the REAL widget: the TP/SL toggle is disabled
/// BEFORE any click and its hover says the lane reason the glue stated (I-1, step 3, end to
/// end), and the account picker of a window whose node did not answer the directory says
/// "Account list unavailable." (the owner's decision of 10-03, item 5). CONTROL: a perp engine's
/// toggle can be turned on.
#[test]
fn the_shipped_daemon_shape_reaches_the_widget_with_its_reasons() {
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT, Queryable};
    let frame = |symbol: &'static str| {
        move |ui: &mut egui::Ui, state: &mut vike_panels::trade::TradeState| {
            if !vike_ui_theme::harness::type_ready(ui.ctx()) {
                return;
            }
            ui.ctx().all_styles_mut(|s| s.interaction.tooltip_delay = 0.0);
            let snap = snap_with(vec![block("binance", None, symbol, Some(AccountTier::Demo))]);
            let cat = synced(&btc_catalog(), &snap, "binance", symbol, "");
            let src = WindowSources {
                directory_unavailable: true,
                bracket_wire: true,
                ..sources(&snap, None, &cat, ("binance", None, symbol))
            };
            let parts = window_parts(&src);
            let _ = vike_panels::trade::draw(ui, state, &parts.inputs());
        }
    };
    let size = vike_ui_theme::value::trade::BESIDE_SIZE;
    let mut h = Harness::builder()
        .with_size(size)
        .build_ui_state(frame("BTCUSDT"), vike_panels::trade::TradeState::default());
    h.run();
    let toggle = h.get_by_label("TP/SL").accesskit_node();
    assert!(toggle.is_disabled(), "TP/SL is off before the click on a spot engine");
    h.hover_at(egui::pos2(1.0, 1.0));
    h.run();
    let before = h.query_all_by_label(TPSL_LANE_WHY).count();
    h.get_by_label("TP/SL").hover();
    h.run();
    assert!(h.query_all_by_label(TPSL_LANE_WHY).count() > before, "the hover says the lane");

    // The strip says it too (nothing more important is on it), so count the picker's line.
    let before = h.query_all_by_label(ACCOUNT_LIST_UNAVAILABLE).count();
    h.get_by_label_contains("binance Spot · main").click();
    h.run();
    assert!(
        h.query_all_by_label(ACCOUNT_LIST_UNAVAILABLE).count() > before,
        "the account picker says the list is unavailable"
    );

    let mut h = Harness::builder()
        .with_size(size)
        .build_ui_state(frame("BTCUSDT.P"), vike_panels::trade::TradeState::default());
    h.run();
    assert!(!h.get_by_label("TP/SL").accesskit_node().is_disabled(), "a perp engine offers it");
}

/// The owner's decision of 10-03, item 5: a failed directory fetch is SAID in the window. The
/// account picker carries "Account list unavailable." (the widget draws it at the top of the
/// list), and the strip says it too where nothing more important is on it; the window stays
/// usable — venues by their keys, the accounts the snapshot runs.
#[test]
fn a_window_whose_node_did_not_answer_the_directory_says_the_list_is_unavailable() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let src = WindowSources {
        directory_unavailable: true,
        ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT"))
    };
    let p = window_parts(&src);
    assert_eq!(p.accounts_why, Some(ACCOUNT_LIST_UNAVAILABLE));
    assert!(p.tradable, "the window stays usable");
    assert_eq!(p.accounts.len(), 2, "the accounts the snapshot runs are listed");
    let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
    assert_eq!(strip, Some((StatusKind::Info, ACCOUNT_LIST_UNAVAILABLE)), "the strip says it");

    // Anything more important on the strip keeps it: here the window's own account is not run.
    let src = WindowSources {
        directory_unavailable: true,
        ..sources(&snap, None, &cat, ("bybit", None, "BTCUSDT"))
    };
    let p = window_parts(&src);
    assert_eq!(p.accounts_why, Some(ACCOUNT_LIST_UNAVAILABLE), "the picker still says it");
    assert_eq!(p.status.as_ref().map(|(_, t)| &**t), Some(NOT_RUNNING_WHY));

    // CONTROL: a node that answered (or one not asked yet) says nothing of the kind.
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    assert_eq!(p.accounts_why, None);
    assert_eq!(p.status, None);
}

/// The owner's decision of 10-03, item 11: a desktop that cannot send — READ-ONLY (no control
/// channel) or with its control link LOST — is a STATED cause up front, in the strip and in
/// `Tradable::No::why`, so every order control says it before the click. Every symbol match is
/// view only there. Their place among the other causes: Error causes before Info ones (an Error
/// outranks the window's own last line on the strip), the widest scope first within each — the
/// server, then this desktop's link, then the window's address, then the account; and among the
/// Info causes this desktop's read-only setting before a node that has said nothing and an
/// account it does not run.
#[test]
fn a_desktop_that_cannot_send_says_so_up_front() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "BTC");
    let at = |snap: &CoreSnapshot, link, account| {
        let src = WindowSources {
            control_link: link,
            ..sources(snap, None, &cat, ("binance", account, "BTCUSDT"))
        };
        let p = window_parts(&src);
        let why = match p.inputs().tradable {
            Tradable::No { why, .. } => why.map(str::to_string),
            Tradable::Yes => None,
        };
        let strip = p.status.as_ref().map(|(k, t)| (*k, t.to_string()));
        (p.tradable, why, strip)
    };
    let read_only = at(&snap, ControlLink::ReadOnly, None);
    assert_eq!(
        read_only,
        (
            false,
            Some(READ_ONLY_WHY.to_string()),
            Some((StatusKind::Info, READ_ONLY_WHY.to_string())),
        )
    );
    let lost = at(&snap, ControlLink::Lost, None);
    assert_eq!(
        lost,
        (
            false,
            Some(NO_CONTROL_LINK_WHY.to_string()),
            Some((StatusKind::Error, NO_CONTROL_LINK_WHY.to_string())),
        )
    );
    assert!(at(&snap, ControlLink::Open, None).0, "CONTROL: a desktop that sends trades");

    // The order among the causes.
    let mut faulted = two_accounts();
    faulted.fault = Some("handler panicked".into());
    let mut halted = two_accounts();
    halted.portfolio.venues[1].trading_state = vike_model::TradingState::Halted;
    let why = |snap: &CoreSnapshot, link, account| at(snap, link, account).1;
    let stopped = stopped_why("handler panicked");
    assert_eq!(why(&faulted, ControlLink::Lost, None), Some(stopped.clone()), "server first");
    assert_eq!(why(&faulted, ControlLink::ReadOnly, None), Some(stopped));
    assert_eq!(
        why(&snap, ControlLink::Lost, Some("sub")),
        Some(NO_CONTROL_LINK_WHY.to_string()),
        "the link before the window's address"
    );
    assert_eq!(
        why(&halted, ControlLink::Lost, Some("SUB")),
        Some(NO_CONTROL_LINK_WHY.to_string()),
        "the link before the account"
    );
    assert_eq!(
        why(&halted, ControlLink::ReadOnly, Some("SUB")),
        Some(HALTED_WHY.to_string()),
        "an Error cause before the read-only setting"
    );
    assert_eq!(
        why(&snap, ControlLink::ReadOnly, Some("sub")),
        Some(UNADDRESSABLE_WHY.to_string()),
        "an Error cause before the read-only setting"
    );
    let waiting = CoreSnapshot::empty("", "");
    assert_eq!(
        why(&waiting, ControlLink::ReadOnly, None),
        Some(READ_ONLY_WHY.to_string()),
        "read-only before a node that has said nothing"
    );
    let elsewhere = WindowSources {
        control_link: ControlLink::ReadOnly,
        ..sources(&snap, None, &cat, ("bybit", None, "BTCUSDT"))
    };
    assert_matches!(
        window_parts(&elsewhere).inputs().tradable,
        Tradable::No { why: Some(READ_ONLY_WHY), .. },
        "read-only before an account the server does not run"
    );
}

/// The shell's one value: a desktop with no control handle is read-only, one whose handle has
/// lost its link cannot send either, and only a connected handle sends.
#[test]
fn the_control_link_is_read_off_the_handle() {
    assert_eq!(ControlLink::of(None), ControlLink::ReadOnly);
    assert_eq!(ControlLink::of(Some(false)), ControlLink::Lost);
    assert_eq!(ControlLink::of(Some(true)), ControlLink::Open);
}

/// Orders follow the PICKED symbol: a demo or live account of a following venue trades any symbol
/// its venue's catalog lists, not only the one its engine was started on — the spot twin and the
/// perpetual alike, told apart by the catalog's own spelling (`BTCUSDT` / `BTCUSDT.P`).
#[test]
fn a_catalog_symbol_the_engine_was_not_started_on_is_tradable() {
    let snap = two_accounts();
    for (symbol, product) in [("BTCUSDC", "Spot"), ("BTCUSDT.P", "Perp")] {
        let cat = synced(&btc_catalog(), &snap, "binance", symbol, "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), symbol)));
        assert!(p.tradable, "{symbol}");
        assert_eq!(p.inputs().tradable, Tradable::Yes, "{symbol}");
        assert_eq!(p.product, product, "{symbol}: the catalog's kind");
        assert_eq!(p.status, None, "{symbol}: nothing to say");
        assert_eq!(p.trades, ["BTCUSDT"], "what the engine was started on is still what it says");
    }
}

/// A symbol the venue's catalog does not list is refused with the catalog as the cause, on the
/// strip and in the widget's cause alike; and before the catalog has loaded it says that instead.
#[test]
fn a_symbol_the_catalog_does_not_list_is_refused_with_that_cause() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "ETHUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "ETHUSDT")));
    assert!(!p.tradable);
    let want = "ETHUSDT is not in binance's instrument catalog, so this account cannot trade it. \
                Refresh the catalog in Data Manager → Instruments.";
    assert_eq!(p.status, Some((StatusKind::Info, Cow::Borrowed(want))));
    assert_matches!(p.inputs().tradable, Tradable::No { why: Some(w), .. } if w == want);

    let empty = Arc::new(Catalog::from_instruments(Vec::new()));
    let cat = synced(&empty, &snap, "binance", "ETHUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "ETHUSDT")));
    assert!(!p.tradable);
    assert_matches!(
        p.status,
        Some((StatusKind::Info, ref w)) if w.starts_with("The instrument catalog has not loaded yet")
    );
}

/// The accounts that keep to one symbol say what they do trade and why — the table's reasons
/// (`symbol_follow`): a paper account, deribit (refused for now), and a venue whose prices are not
/// served yet — and send nothing on another symbol, listed or not.
#[test]
fn an_account_that_keeps_one_symbol_says_what_it_trades_and_why() {
    use crate::orders::symbol_follow::OneSymbol;
    let cases = [
        (block("binance", None, "BTCUSDT", Some(AccountTier::Paper)), OneSymbol::Paper),
        (block("deribit", None, "BTC-PERPETUAL", Some(AccountTier::Live)), OneSymbol::Deribit),
        (block("oanda", None, "EURUSD", Some(AccountTier::Demo)), OneSymbol::PricesNotServed),
        // An OLDER node publishes tier and symbols but not the capability: it would sign a foreign
        // symbol on the mounted one behind a plain Ack, so the window says why it sends nothing.
        (
            VenueBlock {
                admits_symbols: false,
                ..block("binance", None, "BTCUSDT", Some(AccountTier::Live))
            },
            OneSymbol::NodeDoesNotOffer,
        ),
    ];
    for (engine, why) in cases {
        let (venue, mounted) = (engine.venue.clone(), engine.symbol.clone());
        let snap = snap_with(vec![engine]);
        let cat = synced(&btc_catalog(), &snap, &venue, "BTCUSDC", "");
        let p = window_parts(&sources(&snap, None, &cat, (venue.as_str(), None, "BTCUSDC")));
        assert!(!p.tradable, "{venue}");
        let want =
            format!("This account does not trade BTCUSDC. It trades {mounted}. {}", why.why());
        assert_eq!(p.status, Some((StatusKind::Info, Cow::Owned(want.clone()))), "{venue}");
        assert_matches!(p.inputs().tradable, Tradable::No { why: Some(w), .. } if w == want);
    }
}

/// The F wave's reason slot: every cause the glue can STATE for a window that takes no order
/// reaches the widget as `Tradable::No::why` in the very words its status strip says — ONE
/// text, computed once, so the ticket's line, the ladder's hint and the disabled buttons can
/// never word it differently from the strip, and the widget never has to read the strip to
/// guess it. A stopped core (with blocks, and the empty snapshot a fault while publishing
/// leaves), an account text that names no account, a halted account, an account the server
/// does not run, a node that has said nothing yet. With no window status the strip shows the
/// cause itself, an ERROR for the first four kinds and an Info line for the last two.
#[test]
fn each_stated_cause_reaches_the_widget_in_the_strips_own_words() {
    let mut faulted = two_accounts();
    faulted.fault = Some("handler panicked".into());
    let mut faulted_empty = CoreSnapshot::empty("binance", "BTCUSDT");
    faulted_empty.fault = Some("panic during snapshot publish".into());
    let mut halted = two_accounts();
    halted.portfolio.venues[1].trading_state = vike_model::TradingState::Halted;
    let (running, waiting) = (two_accounts(), CoreSnapshot::empty("", ""));
    let mut faulted_and_halted = halted.clone();
    faulted_and_halted.fault = Some("handler panicked".into());
    let error = StatusKind::Error;
    let cases = [
        ("a stopped core", &faulted, ("binance", None), error, stopped_why("handler panicked")),
        (
            "a stopped core, empty snapshot",
            &faulted_empty,
            ("binance", None),
            error,
            stopped_why("panic during snapshot publish"),
        ),
        ("no label", &running, ("binance", Some("sub")), error, UNADDRESSABLE_WHY.to_string()),
        ("a halted account", &halted, ("binance", Some("SUB")), error, HALTED_WHY.to_string()),
        ("not running", &running, ("bybit", None), StatusKind::Info, NOT_RUNNING_WHY.to_string()),
        ("waiting", &waiting, ("binance", None), StatusKind::Info, WAITING_FOR_NODE.to_string()),
        // F2 m5: a core in its safe state with a HALTED account — the two together, as the
        // core leaves them — says the fault's words, never the halt's.
        (
            "a stopped core over a halted account",
            &faulted_and_halted,
            ("binance", Some("SUB")),
            error,
            stopped_why("handler panicked"),
        ),
    ];
    for (what, snap, (venue, account), kind, words) in cases {
        let cat = synced(&btc_catalog(), snap, venue, "BTCUSDT", "");
        let p = window_parts(&sources(snap, None, &cat, (venue, account, "BTCUSDT")));
        assert!(!p.tradable, "{what}");
        match p.inputs().tradable {
            Tradable::No { why, .. } => {
                assert_eq!(why, Some(words.as_str()), "{what}: the widget is told the cause");
            }
            Tradable::Yes => panic!("{what}: the window takes orders"),
        }
        let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
        assert_eq!(strip, Some((kind, words.as_str())), "{what}: the strip says the same");
    }
}

/// The reason slot under the strip's precedence: the window's own last order, note or refusal
/// outranks an Info cause ON THE STRIP (not running, waiting) and the stated cause still reaches
/// the widget — the strip's line is not the widget's cause. An ERROR cause (a stopped core)
/// outranks the window's own line on the strip too. A REDUCING account and an instrument with
/// no lot size are not causes: the window still takes orders (the ticket refuses a size it
/// cannot send in its own words).
#[test]
fn the_stated_cause_is_the_widgets_whatever_the_strip_shows() {
    let rejected: (StatusKind, String) =
        (StatusKind::Error, "Rejected by the venue: insufficient margin.".into());
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "bybit", "BTCUSDT", "");
    let src = WindowSources {
        status: Some(&rejected),
        ..sources(&snap, None, &cat, ("bybit", None, "BTCUSDT"))
    };
    let p = window_parts(&src);
    let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
    assert_eq!(strip, Some((StatusKind::Error, rejected.1.as_str())), "the window's own line");
    assert_matches!(
        p.inputs().tradable,
        Tradable::No { why: Some(NOT_RUNNING_WHY), .. },
        "{:?}",
        p.inputs().tradable
    );

    let mut faulted = two_accounts();
    faulted.fault = Some("handler panicked".into());
    let cat = synced(&btc_catalog(), &faulted, "binance", "BTCUSDT", "");
    let src = WindowSources {
        status: Some(&rejected),
        ..sources(&faulted, None, &cat, ("binance", None, "BTCUSDT"))
    };
    let p = window_parts(&src);
    let stopped = stopped_why("handler panicked");
    let strip = p.status.as_ref().map(|(k, t)| (*k, &**t));
    assert_eq!(strip, Some((StatusKind::Error, stopped.as_str())), "the fault outranks it");
    assert_matches!(p.inputs().tradable, Tradable::No { why: Some(w), .. } if w == stopped);

    let mut reducing = two_accounts();
    reducing.portfolio.venues[1].trading_state = vike_model::TradingState::Reducing;
    let cat = synced(&btc_catalog(), &reducing, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&reducing, None, &cat, ("binance", Some("SUB"), "BTCUSDT")));
    assert_eq!(p.inputs().tradable, Tradable::Yes, "a reducing account still trades");

    let unlisted = snap_with(vec![block("binance", None, "ETHUSDT", Some(AccountTier::Live))]);
    let cat = synced(&btc_catalog(), &unlisted, "binance", "ETHUSDT", "");
    let p = window_parts(&sources(&unlisted, None, &cat, ("binance", None, "ETHUSDT")));
    assert_eq!(p.inputs().tradable, Tradable::Yes, "no lot size is the ticket's to refuse");
}

/// C3: a row on another venue opens THAT venue's spelling of the instrument — the same class
/// and quote where it lists one — never the window's own symbol.
#[test]
fn an_account_row_on_another_venue_opens_that_venues_own_symbol() {
    let snap = snap_with(vec![
        block("okx", None, "BTC-USDT-SWAP", Some(AccountTier::Demo)),
        block("binance", None, "BTCUSDT", Some(AccountTier::Live)),
        block("bybit", None, "BTCUSDT", Some(AccountTier::Paper)),
    ]);
    let cat = synced(&btc_catalog(), &snap, "okx", "BTC-USDT-SWAP", "");
    let p = window_parts(&sources(&snap, None, &cat, ("okx", None, "BTC-USDT-SWAP")));
    let opens: Vec<(&str, &str)> = p.accounts.iter().map(|r| (r.venue, r.symbol)).collect();
    assert_eq!(
        opens,
        [("okx", "BTC-USDT-SWAP"), ("binance", "BTCUSDT.P"), ("bybit", "BTCUSDT")],
        "the swap window opens binance's perp, and bybit's only BTC/USDT pair"
    );

    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    let okx = p.accounts.iter().find(|r| r.venue == "okx").expect("an okx row");
    assert_eq!(okx.symbol, "BTC-USDT", "the spot window opens okx's spot pair");
}

/// C3: a recent pick is an address only with its venue.
#[test]
fn recent_picks_carry_their_venue() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let recent = [("okx".to_string(), "BTC-USDT-SWAP".to_string())];
    let src = WindowSources {
        recent: &recent,
        ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT"))
    };
    assert_eq!(window_parts(&src).recent, [("okx", "BTC-USDT-SWAP")]);
}

/// A1's money-path carry: an account text that is not a label never becomes the DEFAULT
/// account. The window refuses — it trades nothing, offers no bracket, and says why.
#[test]
fn an_account_text_that_is_not_a_label_refuses_the_window() {
    let mut snap = two_accounts();
    snap.orders = vec![order("a1", None, OrderStatus::Accepted)];
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    assert_eq!(TradeAddress::parse("binance", Some("sub"), "BTCUSDT"), None);
    let p = window_parts(&sources(&snap, None, &cat, ("binance", Some("sub"), "BTCUSDT")));
    assert_eq!(p.account, Some("sub"), "the bar shows what the window holds");
    assert!(!p.tradable && p.bracket_why.is_some());
    assert!(p.orders.is_empty(), "the default account's orders are not this window's");
    assert_eq!(p.mode, None);
    assert_eq!(p.status, Some((StatusKind::Error, Cow::Borrowed(UNADDRESSABLE_WHY))));
    // Fix round 1, M-7: the cancel row is worded with the same reason, beyond the carry's
    // `orders_why` formula (which covers Ruling R9 alone).
    assert_eq!(p.orders_why, Some(UNADDRESSABLE_WHY));
}

/// TP/SL is offered only on the symbol the engine was started on: the node judges the lane from the
/// engine's symbol, so on a picked one the window says so before the click.
#[test]
fn tp_sl_is_offered_only_on_the_symbol_the_engine_was_started_on() {
    let snap = snap_with(vec![block("binance", None, "BTCUSDT.P", Some(AccountTier::Live))]);
    for (symbol, why) in [("BTCUSDT.P", None), ("BTCUSDT", Some(TPSL_SYMBOL_WHY))] {
        let cat = synced(&btc_catalog(), &snap, "binance", symbol, "");
        let p = window_parts(&sources(&snap, None, &cat, ("binance", None, symbol)));
        assert_eq!(p.bracket_why, why, "{symbol}");
    }
}
