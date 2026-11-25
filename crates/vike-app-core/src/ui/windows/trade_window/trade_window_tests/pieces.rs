//! The Trade glue's small pieces, and the I13 catalog cache.

use super::*;

// ---- the small pieces ----------------------------------------------------------------------

/// O7: every one of the 14 statuses keeps the colour its word had (FILLED good; REJECTED, DENIED, EXPIRED and
/// LIQUIDATED bad; the rest information), now off the status itself, so a new one cannot fall to Info unseen.
#[test]
fn a_status_line_reads_an_orders_progress() {
    for (status, kind) in [
        (OrderStatus::Initialized, TradeWindowStatusLineKind::Info),
        (OrderStatus::Submitted, TradeWindowStatusLineKind::Info),
        (OrderStatus::Accepted, TradeWindowStatusLineKind::Info),
        (OrderStatus::Triggered, TradeWindowStatusLineKind::Info),
        (OrderStatus::PartiallyFilled, TradeWindowStatusLineKind::Info),
        (OrderStatus::Filled, TradeWindowStatusLineKind::Ok),
        (OrderStatus::Canceled, TradeWindowStatusLineKind::Info),
        (OrderStatus::Rejected, TradeWindowStatusLineKind::Error),
        (OrderStatus::Denied, TradeWindowStatusLineKind::Error),
        (OrderStatus::Expired, TradeWindowStatusLineKind::Error),
        (OrderStatus::PendingCancel, TradeWindowStatusLineKind::Info),
        (OrderStatus::Liquidated, TradeWindowStatusLineKind::Error),
        (OrderStatus::Emulated, TradeWindowStatusLineKind::Info),
        (OrderStatus::Released, TradeWindowStatusLineKind::Info),
    ] {
        assert_eq!(status_of(status), kind, "{status:?}");
        // The colour the old word match gave the same status.
        let by_word = match status.as_str() {
            "FILLED" => TradeWindowStatusLineKind::Ok,
            "REJECTED" | "DENIED" | "EXPIRED" | "LIQUIDATED" => TradeWindowStatusLineKind::Error,
            _ => TradeWindowStatusLineKind::Info,
        };
        assert_eq!(kind, by_word, "{status:?} keeps its colour");
    }
}

#[test]
fn the_seed_is_the_last_used_instrument_else_the_primary_market() {
    let snapshot = snapshot_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    let last =
        TradeWindowPick { venue: "okx".into(), account: None, symbol: "BTC-USDT-SWAP".into() };
    assert_eq!(seed(&snapshot, Some(&last)), Some(last));
    assert_eq!(
        seed(&snapshot, None),
        Some(TradeWindowPick { venue: "binance".into(), account: None, symbol: "BTCUSDT".into() })
    );
}

/// C1: a node whose primary engine is a labelled account opens the first window on THAT
/// account. A window on "binance, default account" there would name a book the server does not
/// run, and an account-less order on it would reach engine 0, the labelled one.
#[test]
fn a_labelled_primary_seeds_a_window_on_its_own_account() {
    let snapshot =
        snapshot_with(vec![block("binance", Some("SUB"), "BTCUSDT", Some(AccountTier::Live))]);
    let pick = seed(&snapshot, None).expect("a primary market");
    assert_eq!(
        pick,
        TradeWindowPick {
            venue: "binance".into(),
            account: Some("SUB".into()),
            symbol: "BTCUSDT".into()
        }
    );
    let address = TradeWindowAddress::parse(&pick.venue, pick.account.as_deref(), &pick.symbol)
        .expect("the seed's label parses");
    assert!(tradable(&snapshot, &address), "the seeded window can trade what it names");
}

/// Minor 27: before the node's first snapshot there is nothing to open, and the window says it
/// is waiting rather than opening on an empty address.
#[test]
fn a_window_waits_for_the_node_rather_than_opening_on_nothing() {
    assert_eq!(seed(&CoreSnapshot::empty("", ""), None), None);
}

/// A venue is named by the settings database's title, else by its key (spec §3.3, the owner's
/// ruling of 2026-09-30). Nothing in this code spells a venue.
#[test]
fn a_venue_is_named_by_the_databases_title_else_by_its_key() {
    let dir = AccountDirectoryWire {
        venues: vec![venue("ctrader", Some("cTrader")), venue("okx", None)],
        accounts: vec![],
    };
    assert_eq!(venue_label(Some(&dir), "ctrader"), "cTrader");
    assert_eq!(venue_label(Some(&dir), "okx"), "okx", "a store not yet carried shows the key");
    assert_eq!(venue_label(None, "binance"), "binance", "an older node shows the key");
}

/// An unlabelled account is "main", unless its venue holds more than one; then the broker's
/// number tells them apart.
#[test]
fn an_account_is_shown_by_label_else_by_book_number_when_two_share_a_venue() {
    let mut a = account(1, "dukascopy", None, "demo");
    a.venue_account_id = Some("DEMO-111".into());
    assert_eq!(account_name(&a, 1), "main");
    assert_eq!(account_name(&a, 2), "DEMO-111");
    let hedge = account(2, "binance", Some("HEDGE"), "demo");
    assert_eq!(account_name(&hedge, 2), "HEDGE");
}

/// The engine the server runs for an address belongs to the one account whose tier its mode
/// names, when two unlabelled accounts share the address (a venue's demo and live books).
#[test]
fn a_running_engine_belongs_to_the_account_whose_tier_its_mode_names() {
    let dir = AccountDirectoryWire {
        venues: vec![venue("hyperliquid", Some("Hyperliquid"))],
        accounts: vec![
            account(4, "hyperliquid", None, "demo"),
            account(5, "hyperliquid", None, "live"),
        ],
    };
    let running = [block("hyperliquid", None, "BTC", Some(AccountTier::Live))];
    let engines = engines_run(&dir, &running);
    assert_eq!(engines.get(&4), None, "the demo book is not running");
    assert_eq!(engines.get(&5), Some(&0), "the live book is");
    // ...and an engine whose tier neither row states goes to the lower id, whichever order the
    // rows came in.
    let paper = [block("hyperliquid", None, "BTC", Some(AccountTier::Paper))];
    let mut reversed = dir.clone();
    reversed.accounts.reverse();
    for d in [&dir, &reversed] {
        let engines = engines_run(d, &paper);
        assert_eq!((engines.get(&4), engines.get(&5)), (Some(&0), None));
    }
    assert_eq!(why_not(true, false, None), Some(NOT_RUNNING_WHY));
    let said = not_running_because("no credentials");
    assert_eq!(said, "Not running on the server: no credentials.");
    assert_eq!(why_not(true, false, Some(&said)), Some(said.as_str()), "the node's reason, said");
    assert_eq!(why_not(false, true, None), Some(NOT_LISTED_WHY), "not listed outranks not running");
    assert_eq!(why_not(false, false, Some(&said)), Some(NOT_LISTED_WHY), "and outranks a reason");
    assert_eq!(NOT_RUNNING_WHY, "Not running on the server.");
    assert!(why_not(true, true, None).is_none());
}

/// An instrument the catalog does not list gets NO quote currency read off its spelling (the owner's
/// ruling of 2026-11-13): the whole symbol is the base, the quote says only that it is the quote.
/// (`BTCUSDT` read as `BTC`/`USDT` and `EURGBP` as `EURGBP`/`USDT` until then.)
#[test]
fn an_unlisted_symbol_guesses_no_quote_currency() {
    for symbol in ["BTCUSDT", "SOLUSDC", "XBTUSD", "EURGBP", "BTC-PERPETUAL", "USDT"] {
        assert_eq!(unlisted_base_quote(symbol), (symbol, UNLISTED_QUOTE));
    }
    let snapshot = snapshot_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    let empty = Arc::new(Catalog::from_instruments(Vec::new()));
    let c = synced(&empty, &snapshot, "binance", "BTCUSDT", "");
    assert_eq!(c.base_quote("BTCUSDT"), ("BTCUSDT", UNLISTED_QUOTE));
    assert_eq!(
        btc_listed("BTCUSDT").base_quote("BTCUSDT"),
        ("BTC", "USDT"),
        "a listed one is read"
    );
}

fn btc_listed(symbol: &str) -> TradeWindowCatalog {
    let snapshot = snapshot_with(vec![block("binance", None, symbol, Some(AccountTier::Live))]);
    synced(&btc_catalog(), &snapshot, "binance", symbol, "")
}

// ---- I13: the catalog answers are cached, and a cache never answers for the wrong catalog ---

/// The grid is read from the catalog the window was handed THIS frame: a refreshed catalog
/// (the shell replaces its `Arc`) and another instrument are both read again, and the symbol
/// picker's matches follow the query and the catalog alike.
#[test]
fn the_catalog_answers_follow_the_instrument_the_query_and_a_refreshed_catalog() {
    let snapshot = snapshot_with(vec![
        block("binance", None, "BTCUSDT", Some(AccountTier::Live)),
        block("okx", None, "BTC-USDT-SWAP", Some(AccountTier::Demo)),
    ]);
    let empty = Arc::new(Catalog::from_instruments(Vec::new()));
    let mut c = TradeWindowCatalog::default();
    sync_on(&mut c, &empty, &snapshot, ("binance", "BTCUSDT", "BTC"));
    assert!(c.listing.is_none() && !c.loaded, "nothing is listed before the catalog loads");
    assert!(c.rows.is_empty());

    let full = btc_catalog();
    sync_on(&mut c, &full, &snapshot, ("binance", "BTCUSDT", "BTC"));
    assert!(c.loaded);
    assert_eq!(
        c.listing.as_ref().and_then(|l| l.grid.lot_size),
        Some(0.001),
        "a new catalog is read"
    );
    assert!(!c.rows.is_empty(), "...and so are the rows for an unchanged query");

    sync_on(&mut c, &full, &snapshot, ("okx", "BTC-USDT-SWAP", "BTC"));
    assert_eq!(
        c.listing.as_ref().and_then(|l| l.grid.lot_size),
        Some(0.01),
        "a new instrument too"
    );

    sync_on(&mut c, &full, &snapshot, ("okx", "BTC-USDT-SWAP", "SWAP"));
    let found: Vec<&str> = c.rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(found, ["BTC-USDT-SWAP"], "a new query searches again");
}

/// Fix round 1, I-1: the other venues' spellings depend on the window instrument's CLASS as well
/// as its base and quote. A window moved from okx's swap to okx's spot pair (same base, same
/// quote) must stop opening binance's PERP from its binance row.
#[test]
fn another_venues_spelling_follows_the_window_instruments_class() {
    let snapshot = two_accounts();
    let cat = btc_catalog();
    let mut c = TradeWindowCatalog::default();
    sync_on(&mut c, &cat, &snapshot, ("okx", "BTC-USDT-SWAP", ""));
    assert_eq!(c.spelling_on("binance"), Some("BTCUSDT.P"), "a swap window opens the perp");
    sync_on(&mut c, &cat, &snapshot, ("okx", "BTC-USDT", ""));
    assert_eq!(c.spelling_on("binance"), Some("BTCUSDT"), "a spot window opens the spot pair");
    sync_on(&mut c, &cat, &snapshot, ("okx", "BTC-USDT-SWAP", ""));
    assert_eq!(c.spelling_on("binance"), Some("BTCUSDT.P"), "...and back");
}

/// The venue switch and the symbol picker share ONE rule for "the same instrument on another venue"
/// (`vike_catalog::same_instrument_per_venue`): a `BTCUSDT` window offered a venue that lists only
/// `BTC-EUR` and `BTC-USDC` (EUR first) opens the USDC pair, the one the picker draws on `BTCUSDT`'s
/// line. It used to take the catalog's first, the EUR pair.
#[test]
fn another_venues_spelling_is_the_one_the_picker_draws_on_the_windows_line() {
    let snapshot = two_accounts();
    let cat = Arc::new(Catalog::from_instruments(vec![
        instrument("binance", "BTCUSDT", "USDT", Contract::CryptoSpot, 0.001),
        instrument("okx", "BTC-EUR", "EUR", Contract::CryptoSpot, 0.0001),
        instrument("okx", "BTC-USDC", "USDC", Contract::CryptoSpot, 0.0001),
    ]));
    let c = synced(&cat, &snapshot, "binance", "BTCUSDT", "");
    assert_eq!(c.spelling_on("okx"), Some("BTC-USDC"));
}

/// Fix round 1, M-5: the window's own listing is an EXACT lookup. A venue whose raw symbols are
/// mixed-case (hyperliquid's `kPEPE`) is listed, not reported as missing with every size
/// refused.
#[test]
fn a_mixed_case_symbol_is_found_in_the_catalog() {
    let snapshot =
        snapshot_with(vec![block("hyperliquid", None, "kPEPE", Some(AccountTier::Live))]);
    let mut pepe = instrument("hyperliquid", "kPEPE", "USDC", linear_perp("USDC"), 1.0);
    pepe.base = "kPEPE".into();
    let cat = Arc::new(Catalog::from_instruments(vec![pepe]));
    let c = synced(&cat, &snapshot, "hyperliquid", "kPEPE", "");
    assert_eq!(c.listing.as_ref().and_then(|l| l.grid.lot_size), Some(1.0));
    let p = trade_window_parts(&sources(&snapshot, None, &c, ("hyperliquid", None, "kPEPE")));
    assert_eq!(p.grid.lot_size, Some(1.0));
    assert_eq!(p.status, None, "nothing is missing: {:?}", p.status);
}

/// A catalog row from the datahub's venue list carries no grid (every tick and lot is zero). The
/// window's price and ladder then take the BOOK's own tick; a positive catalog tick still wins.
#[test]
fn a_catalog_row_with_no_grid_takes_the_books_tick() {
    let snapshot = snapshot_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    let mut bare = instrument("binance", "BTCUSDT", "USDT", Contract::CryptoSpot, 0.0);
    bare.properties = SymbolProperties::default();
    let cat = Arc::new(Catalog::from_instruments(vec![bare]));
    let c = synced(&cat, &snapshot, "binance", "BTCUSDT", "");
    let mut book = L2Book::new(0.01);
    book.apply_snapshot(
        1,
        &[vike_marketdata::L2BookLevel::new(100.0, 1.0)],
        &[vike_marketdata::L2BookLevel::new(100.01, 1.0)],
    );
    let src = TradeWindowSources {
        book: Some(&book),
        ..sources(&snapshot, None, &c, ("binance", None, "BTCUSDT"))
    };
    let p = trade_window_parts(&src);
    assert_eq!(p.grid.tick_size, Some(0.01), "the book's tick answers where the catalog's is zero");
    assert_eq!(p.grid.lot_size, None, "the lot stays unknown, never invented");
    // With a grid in the catalog, the catalog's tick wins.
    let cat = btc_catalog();
    let c = synced(&cat, &snapshot, "binance", "BTCUSDT", "");
    let src = TradeWindowSources {
        book: Some(&book),
        ..sources(&snapshot, None, &c, ("binance", None, "BTCUSDT"))
    };
    assert_eq!(trade_window_parts(&src).grid.tick_size, Some(0.1));
}

/// W3-E, two accepted changes on garbage input: the window's grid holds a number that is not a
/// positive, finite number as NONE (`vike_order_entry::trade_window::grid_value`).
/// - A catalog lot of `-0.001` (or `NaN`, an infinity, `0`) is no lot: the window refuses every
///   size on it, as it did, and its size field seeds `0` where the raw `-0.001` printed `0.000`.
/// - With no catalog tick, a NEGATIVE quote tick is no tick either: the window prints its prices
///   whole, `65,432`, where the raw `-0.01` printed two decimals, `65,432.37`.
#[test]
fn a_catalog_or_quote_number_that_is_not_a_positive_step_is_no_grid() {
    use crate::data::data_sink::TickStore;
    use vike_order_entry::trade_window::ladder::fmt_price;

    let snapshot = snapshot_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    for lot in [-0.001, 0.0, f64::NAN, f64::INFINITY] {
        let row = instrument("binance", "BTCUSDT", "USDT", Contract::CryptoSpot, lot);
        let c = synced(
            &Arc::new(Catalog::from_instruments(vec![row])),
            &snapshot,
            "binance",
            "BTCUSDT",
            "",
        );
        let grid = c.listing.as_ref().map(|l| l.grid).expect("listed");
        assert_eq!((grid.lot_size, grid.min_qty), (None, None), "lot {lot}");
        assert_eq!(grid.tick_size, Some(0.1), "lot {lot}: the catalog's tick stands");
        let p = trade_window_parts(&sources(&snapshot, None, &c, ("binance", None, "BTCUSDT")));
        assert_eq!(p.grid.lot_size, None, "lot {lot}");
    }

    let snapshot = snapshot_with(vec![block("ibkr", None, "AAPL", Some(AccountTier::Demo))]);
    let c = synced(&Arc::new(Catalog::from_instruments(vec![])), &snapshot, "ibkr", "AAPL", "");
    let store = TickStore::default();
    store.set_quote("ibkr", "AAPL", (65_432.37, 65_432.37), 1, None);
    let mut quote = store.quote("ibkr", "AAPL").expect("stored");
    quote.tick = -0.01;
    let src = TradeWindowSources {
        top: Some((quote, 0)),
        ..sources(&snapshot, None, &c, ("ibkr", None, "AAPL"))
    };
    let p = trade_window_parts(&src);
    assert_eq!(p.grid.tick_size, None, "a negative quote tick is no tick");
    let last = p.last.expect("the quote's mid");
    assert_eq!(fmt_price(last, p.grid.tick_size), "65,432", "printed whole, as with no tick");
}

/// **A venue's quote reaches the window as what it is**: a price and a spread with no ladder, under
/// words that say so, on the step the prices were spelled to where the catalog has none, and a `DELAYED`
/// stream says so twice over: on the ladder's quote and on the strip. Reddens on `top` going, on the
/// label dropped, and on the grid left at zero, which prints every price of a five-digit pair as `1`.
#[test]
fn a_quote_is_the_windows_price_with_no_ladder_and_a_delayed_one_says_so() {
    use super::super::reasons::QUOTE_ONLY_HEADLINE;
    use crate::data::data_sink::{TickQuoteView, TickStore};
    use vike_model::StreamDelay;

    let snapshot = snapshot_with(vec![block("ibkr", None, "AAPL", Some(AccountTier::Demo))]);
    let cat = Arc::new(Catalog::from_instruments(vec![]));
    let c = synced(&cat, &snapshot, "ibkr", "AAPL", "");
    let store = TickStore::default();
    store.set_quote("ibkr", "AAPL", (190.1, 190.25), 1, Some(StreamDelay::DELAYED));
    let quote = store.quote("ibkr", "AAPL").expect("stored");

    let src = TradeWindowSources {
        top: Some((quote, 3_000)),
        ..sources(&snapshot, None, &c, ("ibkr", None, "AAPL"))
    };
    let p = trade_window_parts(&src);
    let face = p.top.expect("the widget gets the quote");
    assert_eq!((face.bid, face.ask, face.age_ms), (190.1, 190.25, 3_000));
    assert_eq!(face.delay, Some("DELAYED"));
    assert_eq!(p.last, Some((190.1 + 190.25) / 2.0), "the window's price is the quote's mid");
    assert!(p.book.is_none(), "a quote is no book: the honesty contract draws no ladder from it");
    assert_eq!(p.absence.expect("a bookless window says why").headline, QUOTE_ONLY_HEADLINE);
    assert_eq!(
        p.grid.tick_size,
        Some(0.01),
        "the spelled step answers where the catalog's is zero"
    );
    assert!(
        p.status.as_ref().is_some_and(|(_, text)| text.contains("DELAYED")),
        "the strip says it where the ladder is hidden: {:?}",
        p.status
    );

    // A real-time stream wears no label, and one side alone is no price.
    let mut real_time: TickQuoteView = quote;
    real_time.delay = Some(StreamDelay::RealTime);
    let src = TradeWindowSources {
        top: Some((real_time, 0)),
        ..sources(&snapshot, None, &c, ("ibkr", None, "AAPL"))
    };
    let p = trade_window_parts(&src);
    assert_eq!(p.top.expect("a quote").delay, None);
    let mut one_sided = quote;
    one_sided.bid = 0.0;
    let src = TradeWindowSources {
        top: Some((one_sided, 0)),
        ..sources(&snapshot, None, &c, ("ibkr", None, "AAPL"))
    };
    let p = trade_window_parts(&src);
    assert!(p.top.is_none() && p.last.is_none(), "a dash, never half a price");
}

/// **The window's depth link reaches the widget as its producer set it.** The bookless headline is
/// the line's STATE and the cause is its words, verbatim: a live datahub on a host named
/// `error-box` awaits a snapshot, never a fault, and the widget's FEED badge gets the same line. A
/// venue with no line says no feed serves it, and the widget gets none.
#[test]
fn the_depth_link_reaches_the_window_as_its_producer_set_it() {
    use vike_model::feed_status::{DataClientConnectionState, DataClientStatus};
    use vike_order_entry::trade_window::ladder::absence::{
        TradeWindowLadderAbsenceCause, depth_absence_headline,
    };

    let snapshot = snapshot_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Demo))]);
    let c = synced(&btc_catalog(), &snapshot, "binance", "BTCUSDT", "");
    let line = DataClientStatus::new(
        DataClientConnectionState::Connected,
        "datahub error-box:7878 — 1/1 stream(s) live",
    );
    let src = TradeWindowSources {
        source: Some(&line),
        ..sources(&snapshot, None, &c, ("binance", None, "BTCUSDT"))
    };
    let p = trade_window_parts(&src);
    let absence = p.absence.expect("a bookless window says why");
    let awaiting = depth_absence_headline(TradeWindowLadderAbsenceCause::NoSnapshot);
    assert_eq!(absence.headline, awaiting);
    assert_eq!(absence.cause, line.text, "the line's own words");
    assert_eq!(p.inputs().source, Some(&line), "the badge reads the same line");

    let p = trade_window_parts(&sources(&snapshot, None, &c, ("binance", None, "BTCUSDT")));
    let absence = p.absence.expect("a bookless window says why");
    let no_feed = depth_absence_headline(TradeWindowLadderAbsenceCause::NoFeed);
    assert_eq!(absence.headline, no_feed);
    assert_eq!(p.inputs().source, None);
}

/// A book outranks a quote: the window has a ladder to read, so the quote draws nothing beside it.
#[test]
fn a_book_outranks_a_quote_on_the_same_window() {
    use crate::data::data_sink::TickStore;

    let snapshot = snapshot_with(vec![block("hyperliquid", None, "BTC", Some(AccountTier::Demo))]);
    let cat = Arc::new(Catalog::from_instruments(vec![]));
    let c = synced(&cat, &snapshot, "hyperliquid", "BTC", "");
    let store = TickStore::default();
    store.set_quote("hyperliquid", "BTC", (1.0, 3.0), 1, None);
    let mut book = L2Book::new(1.0);
    book.apply_snapshot(
        1,
        &[vike_marketdata::L2BookLevel::new(99.0, 1.0)],
        &[vike_marketdata::L2BookLevel::new(101.0, 1.0)],
    );
    let src = TradeWindowSources {
        book: Some(&book),
        top: store.quote("hyperliquid", "BTC").map(|q| (q, 0)),
        ..sources(&snapshot, None, &c, ("hyperliquid", None, "BTC"))
    };
    let p = trade_window_parts(&src);
    assert!(p.top.is_none());
    assert_eq!(p.last, Some(100.0), "the book's mid");
}

/// The picker searches EVERY venue the catalog lists, one row per instrument, ranked: a query
/// that a great many instruments of one venue match still shows another venue's.
#[test]
fn the_picker_lists_every_venue_flat_and_ranked() {
    let snapshot = snapshot_with(vec![block("binance", None, "BTCUSDC", Some(AccountTier::Live))]);
    let mut items: Vec<Instrument> = (0..250)
        .map(|n| instrument("bybit", &format!("BTC{n}USDT"), "USDT", Contract::CryptoSpot, 0.1))
        .collect();
    items.push(instrument("binance", "BTCUSDC", "USDC", Contract::CryptoSpot, 0.001));
    let cat = Arc::new(Catalog::from_instruments(items));
    let c = synced(&cat, &snapshot, "binance", "BTCUSDC", "BTCUSDC");
    let p = trade_window_parts(&sources(&snapshot, None, &c, ("binance", None, "BTCUSDC")));
    assert_eq!(p.matches.len(), 1, "an exact query finds the one instrument");
    assert_eq!((p.matches[0].venue, p.matches[0].symbol), ("binance", "BTCUSDC"));
    assert!(p.matches[0].current, "it is the window's own instrument");
    assert_eq!(p.matches[0].pair, "BTC/USDC spot");
    let c = synced(&cat, &snapshot, "binance", "BTCUSDC", "BTC");
    let p = trade_window_parts(&sources(&snapshot, None, &c, ("binance", None, "BTCUSDC")));
    assert!(p.matches.len() > 1 && p.matches.len() <= ROWS_CACHED, "{}", p.matches.len());
}

/// The account menu lists EVERY account: one whose venue does not list the window's instrument is
/// a greyed row with the reason, never a missing one, and its symbol is never read.
#[test]
fn an_account_on_a_venue_that_does_not_list_the_instrument_is_a_greyed_row() {
    let snapshot = snapshot_with(vec![
        block("binance", None, "BTCUSDT", Some(AccountTier::Live)),
        block("aster", None, "BTCUSDT", Some(AccountTier::Paper)),
    ]);
    let cat = synced(&btc_catalog(), &snapshot, "binance", "BTCUSDT", "");
    let p = trade_window_parts(&sources(&snapshot, None, &cat, ("binance", None, "BTCUSDT")));
    let rows: Vec<(&str, &str, Option<&str>)> =
        p.accounts.iter().map(|r| (r.venue, r.symbol, r.why_not)).collect();
    assert_eq!(rows, [("binance", "BTCUSDT", None), ("aster", "", Some(NOT_LISTED_WHY))]);
}

/// Review Focus 4: a window already showing something the picker does not offer (a restored
/// option) still builds — no chip is current, the account menu still lists the account.
#[test]
fn a_window_on_an_instrument_the_picker_does_not_offer_still_builds() {
    let snapshot =
        snapshot_with(vec![block("okx", None, "BTC-27NOV26-58000-C", Some(AccountTier::Demo))]);
    let cat = synced(&btc_catalog(), &snapshot, "okx", "BTC-27NOV26-58000-C", "BTC");
    let p =
        trade_window_parts(&sources(&snapshot, None, &cat, ("okx", None, "BTC-27NOV26-58000-C")));
    assert!(p.matches.iter().all(|r| !r.current), "nothing is current");
    assert!(!p.accounts.is_empty(), "the account is still listed");
    assert_eq!(p.symbol, "BTC-27NOV26-58000-C");
}
