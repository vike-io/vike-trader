//! The Trade glue's small pieces, and the I13 catalog cache.

use super::*;

// ---- the small pieces ----------------------------------------------------------------------

#[test]
fn a_status_line_reads_an_orders_progress() {
    assert_eq!(status_of("FILLED"), StatusKind::Ok);
    assert_eq!(status_of("REJECTED"), StatusKind::Error);
    assert_eq!(status_of("ACCEPTED"), StatusKind::Info);
}

#[test]
fn the_seed_is_the_last_used_instrument_else_the_primary_market() {
    let snap = snap_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    let last = TradePick { venue: "okx".into(), account: None, symbol: "BTC-USDT-SWAP".into() };
    assert_eq!(seed(&snap, Some(&last)), Some(last));
    assert_eq!(
        seed(&snap, None),
        Some(TradePick { venue: "binance".into(), account: None, symbol: "BTCUSDT".into() })
    );
}

/// C1: a node whose primary engine is a labelled account opens the first window on THAT
/// account. A window on "binance, default account" there would name a book the server does not
/// run, and an account-less order on it would reach engine 0, the labelled one.
#[test]
fn a_labelled_primary_seeds_a_window_on_its_own_account() {
    let snap = snap_with(vec![block("binance", Some("SUB"), "BTCUSDT", Some(AccountTier::Live))]);
    let pick = seed(&snap, None).expect("a primary market");
    assert_eq!(
        pick,
        TradePick {
            venue: "binance".into(),
            account: Some("SUB".into()),
            symbol: "BTCUSDT".into()
        }
    );
    let addr = TradeAddress::parse(&pick.venue, pick.account.as_deref(), &pick.symbol)
        .expect("the seed's label parses");
    assert!(tradable(&snap, &addr), "the seeded window can trade what it names");
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
    let dir = WireDirectory {
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
    let dir = WireDirectory {
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

#[test]
fn split_base_quote_prefers_the_longest_quote_suffix() {
    // USDT/USDC must win over the USD prefix of their own names.
    assert_eq!(split_base_quote("BTCUSDT"), ("BTC", "USDT"));
    assert_eq!(split_base_quote("SOLUSDC"), ("SOL", "USDC"));
    assert_eq!(split_base_quote("XBTUSD"), ("XBT", "USD"));
}

#[test]
fn split_base_quote_defaults_an_unknown_quote_to_usdt() {
    assert_eq!(split_base_quote("EURGBP"), ("EURGBP", "USDT"));
    assert_eq!(split_base_quote("BTC-PERPETUAL"), ("BTC-PERPETUAL", "USDT"));
}

#[test]
fn split_base_quote_never_yields_an_empty_base() {
    // a symbol that IS only the quote suffix keeps the whole symbol as the base label
    assert_eq!(split_base_quote("USDT"), ("USDT", "USDT"));
    assert_eq!(split_base_quote("USD"), ("USD", "USD"));
}

// ---- I13: the catalog answers are cached, and a cache never answers for the wrong catalog ---

/// The grid is read from the catalog the window was handed THIS frame: a refreshed catalog
/// (the shell replaces its `Arc`) and another instrument are both read again, and the symbol
/// picker's matches follow the query and the catalog alike.
#[test]
fn the_catalog_answers_follow_the_instrument_the_query_and_a_refreshed_catalog() {
    let snap = snap_with(vec![
        block("binance", None, "BTCUSDT", Some(AccountTier::Live)),
        block("okx", None, "BTC-USDT-SWAP", Some(AccountTier::Demo)),
    ]);
    let empty = Arc::new(Catalog::from_instruments(Vec::new()));
    let mut c = TradeCatalog::default();
    sync_on(&mut c, &empty, &snap, ("binance", "BTCUSDT", "BTC"));
    assert!(c.listing.is_none() && !c.loaded, "nothing is listed before the catalog loads");
    assert!(c.rows.is_empty());

    let full = btc_catalog();
    sync_on(&mut c, &full, &snap, ("binance", "BTCUSDT", "BTC"));
    assert!(c.loaded);
    assert_eq!(c.listing.as_ref().map(|l| l.grid.lot), Some(0.001), "a new catalog is read");
    assert!(!c.rows.is_empty(), "...and so are the rows for an unchanged query");

    sync_on(&mut c, &full, &snap, ("okx", "BTC-USDT-SWAP", "BTC"));
    assert_eq!(c.listing.as_ref().map(|l| l.grid.lot), Some(0.01), "a new instrument too");

    sync_on(&mut c, &full, &snap, ("okx", "BTC-USDT-SWAP", "SWAP"));
    let found: Vec<&str> = c.rows.iter().map(|r| r.symbol.as_str()).collect();
    assert_eq!(found, ["BTC-USDT-SWAP"], "a new query searches again");
}

/// Fix round 1, I-1: the other venues' spellings depend on the window instrument's CLASS as well
/// as its base and quote. A window moved from okx's swap to okx's spot pair (same base, same
/// quote) must stop opening binance's PERP from its binance row.
#[test]
fn another_venues_spelling_follows_the_window_instruments_class() {
    let snap = two_accounts();
    let cat = btc_catalog();
    let mut c = TradeCatalog::default();
    sync_on(&mut c, &cat, &snap, ("okx", "BTC-USDT-SWAP", ""));
    assert_eq!(c.spelling_on("binance"), Some("BTCUSDT.P"), "a swap window opens the perp");
    sync_on(&mut c, &cat, &snap, ("okx", "BTC-USDT", ""));
    assert_eq!(c.spelling_on("binance"), Some("BTCUSDT"), "a spot window opens the spot pair");
    sync_on(&mut c, &cat, &snap, ("okx", "BTC-USDT-SWAP", ""));
    assert_eq!(c.spelling_on("binance"), Some("BTCUSDT.P"), "...and back");
}

/// Fix round 1, M-5: the window's own listing is an EXACT lookup. A venue whose raw symbols are
/// mixed-case (hyperliquid's `kPEPE`) is listed, not reported as missing with every size
/// refused.
#[test]
fn a_mixed_case_symbol_is_found_in_the_catalog() {
    let snap = snap_with(vec![block("hyperliquid", None, "kPEPE", Some(AccountTier::Live))]);
    let mut pepe = inst("hyperliquid", "kPEPE", "USDC", linear_perp("USDC"), 1.0);
    pepe.base = "kPEPE".into();
    let cat = Arc::new(Catalog::from_instruments(vec![pepe]));
    let c = synced(&cat, &snap, "hyperliquid", "kPEPE", "");
    assert_eq!(c.listing.as_ref().map(|l| l.grid.lot), Some(1.0));
    let p = window_parts(&sources(&snap, None, &c, ("hyperliquid", None, "kPEPE")));
    assert_eq!(p.grid.lot, 1.0);
    assert_eq!(p.status, None, "nothing is missing: {:?}", p.status);
}

/// A catalog row from the datahub's venue list carries no grid (every tick and lot is zero). The
/// window's price and ladder then take the BOOK's own tick; a positive catalog tick still wins.
#[test]
fn a_catalog_row_with_no_grid_takes_the_books_tick() {
    let snap = snap_with(vec![block("binance", None, "BTCUSDT", Some(AccountTier::Live))]);
    let mut bare = inst("binance", "BTCUSDT", "USDT", Contract::CryptoSpot, 0.0);
    bare.properties = SymbolProperties::default();
    let cat = Arc::new(Catalog::from_instruments(vec![bare]));
    let c = synced(&cat, &snap, "binance", "BTCUSDT", "");
    let mut book = L2Book::new(0.01);
    book.apply_snapshot(
        1,
        &[vike_marketdata::BookLevel::new(100.0, 1.0)],
        &[vike_marketdata::BookLevel::new(100.01, 1.0)],
    );
    let src = WindowSources {
        book: Some(&book),
        ..sources(&snap, None, &c, ("binance", None, "BTCUSDT"))
    };
    let p = window_parts(&src);
    assert_eq!(p.grid.tick, 0.01, "the book's tick answers where the catalog's is zero");
    assert_eq!(p.grid.lot, 0.0, "the lot stays unknown, never invented");
    // With a grid in the catalog, the catalog's tick wins.
    let cat = btc_catalog();
    let c = synced(&cat, &snap, "binance", "BTCUSDT", "");
    let src = WindowSources {
        book: Some(&book),
        ..sources(&snap, None, &c, ("binance", None, "BTCUSDT"))
    };
    assert_eq!(window_parts(&src).grid.tick, 0.1);
}

/// The picker searches EVERY venue the catalog lists, one row per instrument, ranked: a query
/// that a great many instruments of one venue match still shows another venue's.
#[test]
fn the_picker_lists_every_venue_flat_and_ranked() {
    let snap = snap_with(vec![block("binance", None, "BTCUSDC", Some(AccountTier::Live))]);
    let mut items: Vec<Instrument> = (0..250)
        .map(|n| inst("bybit", &format!("BTC{n}USDT"), "USDT", Contract::CryptoSpot, 0.1))
        .collect();
    items.push(inst("binance", "BTCUSDC", "USDC", Contract::CryptoSpot, 0.001));
    let cat = Arc::new(Catalog::from_instruments(items));
    let c = synced(&cat, &snap, "binance", "BTCUSDC", "BTCUSDC");
    let p = window_parts(&sources(&snap, None, &c, ("binance", None, "BTCUSDC")));
    assert_eq!(p.matches.len(), 1, "an exact query finds the one instrument");
    assert_eq!((p.matches[0].venue, p.matches[0].symbol), ("binance", "BTCUSDC"));
    assert!(p.matches[0].current, "it is the window's own instrument");
    assert_eq!(p.matches[0].pair, "BTC/USDC spot");
    let c = synced(&cat, &snap, "binance", "BTCUSDC", "BTC");
    let p = window_parts(&sources(&snap, None, &c, ("binance", None, "BTCUSDC")));
    assert!(p.matches.len() > 1 && p.matches.len() <= ROWS_CACHED, "{}", p.matches.len());
}

/// The account menu lists EVERY account: one whose venue does not list the window's instrument is
/// a greyed row with the reason, never a missing one, and its symbol is never read.
#[test]
fn an_account_on_a_venue_that_does_not_list_the_instrument_is_a_greyed_row() {
    let snap = snap_with(vec![
        block("binance", None, "BTCUSDT", Some(AccountTier::Live)),
        block("aster", None, "BTCUSDT", Some(AccountTier::Paper)),
    ]);
    let cat = synced(&btc_catalog(), &snap, "binance", "BTCUSDT", "");
    let p = window_parts(&sources(&snap, None, &cat, ("binance", None, "BTCUSDT")));
    let rows: Vec<(&str, &str, Option<&str>)> =
        p.accounts.iter().map(|r| (r.venue, r.symbol, r.why_not)).collect();
    assert_eq!(rows, [("binance", "BTCUSDT", None), ("aster", "", Some(NOT_LISTED_WHY))]);
}

/// Review Focus 4: a window already showing something the picker does not offer (a restored
/// option) still builds — no chip is current, the account menu still lists the account.
#[test]
fn a_window_on_an_instrument_the_picker_does_not_offer_still_builds() {
    let snap = snap_with(vec![block("okx", None, "BTC-27NOV26-58000-C", Some(AccountTier::Demo))]);
    let cat = synced(&btc_catalog(), &snap, "okx", "BTC-27NOV26-58000-C", "BTC");
    let p = window_parts(&sources(&snap, None, &cat, ("okx", None, "BTC-27NOV26-58000-C")));
    assert!(p.matches.iter().all(|r| !r.current), "nothing is current");
    assert!(!p.accounts.is_empty(), "the account is still listed");
    assert_eq!(p.symbol, "BTC-27NOV26-58000-C");
}
