//! Fix round 1: a stopped core, a halted account, an account the server does not run.

use super::*;

// ---- fix round 1: a stopped core, a halted account, an account the server does not run ----

/// I-2: a core that faulted while publishing leaves a snapshot with NO blocks and its fault.
/// The window says the server stopped, never "Waiting for the node.", and trades nothing.
#[test]
fn a_faulted_empty_snapshot_says_the_server_stopped() {
    let mut snap = CoreSnapshot::empty("binance", "BTCUSDT");
    snap.fault = Some("panic during snapshot publish".into());
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    assert!(!p.tradable);
    let (kind, text) = p.status.expect("the fault is on the strip");
    assert_eq!(kind, StatusKind::Error);
    assert!(text.contains("panic during snapshot publish"), "{text}");
    assert_ne!(text, WAITING_FOR_NODE);
}

/// I-2: a core in safe state keeps publishing its blocks. Every window on it trades nothing and
/// says why, ahead of any order status, and its matches are all view only.
#[test]
fn a_faulted_core_with_blocks_trades_nothing_and_says_why() {
    let mut snap = two_accounts();
    snap.fault = Some("handler panicked".into());
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "BTC");
    let filled = (StatusKind::Ok, "Buy 1 limit · FILLED".to_string());
    let src = WindowSources {
        status: Some(&filled),
        ..sources(&snap, None, &cat, ("binance", None, "BTCUSDT"))
    };
    let p = window_parts(&src);
    assert!(!p.tradable, "the core halts every order");
    let (kind, text) = p.status.expect("the fault is on the strip");
    assert_eq!(kind, StatusKind::Error);
    assert!(text.contains("handler panicked"), "{text}");
}

/// I-2: an account whose engine is HALTED trades nothing from its window and says so; the
/// other account of the same venue is unaffected. A REDUCING one still trades, and says it takes
/// only orders that reduce its position.
#[test]
fn a_halted_account_trades_nothing_and_a_reducing_one_says_so() {
    let mut snap = two_accounts();
    snap.portfolio.venues[1].trading_state = vike_model::TradingState::Halted;
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let at = |account| window_parts(&sources(&snap, None, &cat, ("binance", account, "BTCUSDT")));
    let sub = at(Some("SUB"));
    assert!(!sub.tradable);
    let (kind, text) = sub.status.expect("the halt is on the strip");
    assert_eq!(kind, StatusKind::Error);
    assert!(text.contains("halted"), "{text}");
    assert!(at(None).tradable, "the default account still trades");

    snap.portfolio.venues[1].trading_state = vike_model::TradingState::Reducing;
    let sub = window_parts(&sources(&snap, None, &cat, ("binance", Some("SUB"), "BTCUSDT")));
    assert!(sub.tradable, "a reducing account still closes and reduces");
    let (kind, text) = sub.status.expect("the strip says what it takes");
    assert_eq!(kind, StatusKind::Info);
    assert!(text.contains("reduce"), "{text}");
}

/// M-1: no primary block, no seed — the window waits rather than folding "unknown" into the
/// default account.
#[test]
fn a_snapshot_with_no_primary_block_seeds_nothing() {
    assert_eq!(seed(&CoreSnapshot::empty("binance", "BTCUSDT"), None), None);
}

/// M-1: a window left on an account the server does not run says so on the strip.
#[test]
fn a_window_on_an_account_the_server_does_not_run_says_so() {
    let snap = two_accounts();
    let cat = synced(&btc_catalog(), &snap, "bybit", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("bybit", None, "BTCUSDT")));
    assert!(!p.tradable);
    assert_eq!(p.status, Some((StatusKind::Info, Cow::Borrowed(NOT_RUNNING_WHY))));
}

/// M-2: a row's account is a ROUTING answer (picking it addresses orders), so it is read from
/// the block's route key. A block whose label did not survive the wire offers no row: as
/// "main" it would route to the default account.
#[test]
fn a_block_with_a_malformed_label_offers_no_row() {
    let mut snap = two_accounts();
    snap.portfolio.venues.push(VenueBlock {
        venue: "binance".into(),
        account: None,
        route_key: "binance#sub".into(),
        symbol: "BTCUSDT".into(),
        mode: Some(EngineMode::Live),
        ..Default::default()
    });
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

/// M-3(b): a picker row never reads PAPER for a mode nobody reported: a running block that
/// publishes none, and a database tier word this build does not know, are both unknown.
#[test]
fn a_row_with_no_known_mode_reads_unknown() {
    let snap = snap_with(vec![block("binance", None, "BTCUSDT", None)]);
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    assert_eq!(p.accounts[0].mode, AccountMode::Unknown);

    let dir = WireDirectory {
        venues: vec![venue("binance", None)],
        accounts: vec![
            account(1, "binance", None, "live"),
            account(2, "binance", Some("X"), "staging"),
        ],
    };
    let p = window_parts(&sources(&snap, Some(&dir), &cat, ("binance", None, "BTCUSDT")));
    let x = p.accounts.iter().find(|r| r.account == Some("X")).expect("the X row");
    assert_eq!((x.mode, x.why_not), (AccountMode::Unknown, Some(NOT_RUNNING_WHY)));
}

/// M-3(c, d): two unlabelled accounts of one venue are told apart by the broker's number (the
/// one without a number stays "main"), the engine goes to the one its mode names, and the
/// labelled rows keep the database's order after the default ones.
#[test]
fn two_unlabelled_accounts_of_one_venue_are_told_apart_and_labelled_rows_keep_their_order() {
    let snap = snap_with(vec![block("dukascopy", None, "EURUSD", Some(EngineMode::Demo))]);
    let cat = synced(&btc_catalog(), &snap, "dukascopy", "EURUSD", "");
    let mut demo = account(7, "dukascopy", None, "demo");
    demo.venue_account_id = Some("DEMO-111".into());
    let dir = WireDirectory {
        venues: vec![venue("dukascopy", Some("Dukascopy"))],
        accounts: vec![
            account(5, "dukascopy", Some("ZED"), "demo"),
            demo,
            account(6, "dukascopy", Some("ALPHA"), "demo"),
            account(8, "dukascopy", None, "live"),
        ],
    };
    let p = window_parts(&sources(&snap, Some(&dir), &cat, ("dukascopy", None, "EURUSD")));
    let rows: Vec<_> = p.accounts.iter().map(|r| (r.account, r.name, r.why_not)).collect();
    assert_eq!(
        rows,
        [
            (None, "DEMO-111", None),
            (None, "main", Some(NOT_RUNNING_WHY)),
            (Some("ZED"), "ZED", Some(NOT_RUNNING_WHY)),
            (Some("ALPHA"), "ALPHA", Some(NOT_RUNNING_WHY)),
        ]
    );
}

/// I18: a symbol the catalog does not list gets no lot size — none is invented — and the
/// window says what is missing instead of a size refusal with a `(0)` in it.
#[test]
fn a_symbol_the_catalog_does_not_list_says_so_and_invents_no_lot() {
    let snap = snap_with(vec![block("binance", None, "ETHUSDT", Some(EngineMode::Live))]);
    let cat = synced(&btc_catalog(), &snap, "binance", "ETHUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "ETHUSDT")));
    assert_eq!((p.grid.lot, p.grid.min_qty), (0.0, 0.0));
    let (kind, text) = p.status.expect("the window says why it cannot size an order");
    assert_eq!(kind, StatusKind::Info);
    assert!(text.contains("the instrument catalog does not list it"), "{text}");
    assert!(!text.contains("(0)"), "{text}");

    let empty =
        synced(&Arc::new(Catalog::from_instruments(Vec::new())), &snap, "binance", "ETHUSDT", "");
    let p = window_parts(&sources(&snap, None, &empty, ("binance", None, "ETHUSDT")));
    let (_, text) = p.status.expect("a reason");
    assert!(text.contains("has not loaded"), "{text}");
}

/// Minor 27: before the node's first snapshot the window says it is waiting.
#[test]
fn a_window_on_a_node_that_has_said_nothing_says_it_is_waiting() {
    let snap = CoreSnapshot::empty("", "");
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    assert_eq!(p.status, Some((StatusKind::Info, Cow::Borrowed(WAITING_FOR_NODE))));
}
