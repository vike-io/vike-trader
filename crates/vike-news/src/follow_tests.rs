use std::sync::Arc;

use vike_catalog::Catalog;
use vike_datahub_client::tool_feeds::NewsItem;
use vike_model::{Contract, Instrument};

use super::{NewsFollow, NewsFollowCache};

fn headline(title: &str, summary: &str, tags: &[&str]) -> NewsItem {
    NewsItem {
        source: "CoinDesk".into(),
        title: title.into(),
        summary: summary.into(),
        url: String::new(),
        market: "crypto".into(),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        timestamp_ms: 0,
    }
}

fn follow(ticker: &str, names: &[&str]) -> NewsFollow {
    NewsFollow { ticker: ticker.into(), names: names.iter().map(|n| n.to_string()).collect() }
}

/// The ticker is a whole word in its upper-case form: "one-day" never names `ONE`, a longer word
/// never names a ticker it starts or ends with.
#[test]
fn the_ticker_is_a_whole_upper_case_word() {
    let one = follow("ONE", &[]);
    assert!(!one.is_named_in(&headline("A one-day rally lifts every coin", "", &[])));
    assert!(!one.is_named_in(&headline("Harmony One upgrades its bridge", "", &[])));
    assert!(one.is_named_in(&headline("Harmony (ONE) upgrades its bridge", "", &[])));
    let btc = follow("BTC", &[]);
    assert!(btc.is_named_in(&headline("BTC's rally stalls", "", &[])));
    assert!(btc.is_named_in(&headline("Markets", "Risk assets rallied, BTC included", &[])));
    assert!(!btc.is_named_in(&headline("WBTC supply slips below BTCUSD's peak", "", &[])));
}

/// A name the asset carries is a whole word in any case: "Bitcoin" names `BTC`.
#[test]
fn a_name_names_the_asset_in_any_case() {
    let btc = follow("BTC", &["Bitcoin"]);
    assert!(btc.is_named_in(&headline("Bitcoin tops $96,000", "", &[])));
    assert!(btc.is_named_in(&headline("Funds pile into BITCOIN", "", &[])));
    assert!(!btc.is_named_in(&headline("Bitcoiners gather in Lisbon", "", &[])));
    assert!(!follow("BTC", &[]).is_named_in(&headline("Bitcoin tops $96,000", "", &[])));
}

/// The feed's own tag wins over the words: an item tagged with the ticker or a name is the asset's
/// whatever its title says, in any case.
#[test]
fn a_tagged_item_wins_over_the_title_words() {
    let btc = follow("BTC", &["Bitcoin"]);
    assert!(btc.is_named_in(&headline("Spot ETF inflows resume", "", &["Markets", "btc"])));
    assert!(btc.is_named_in(&headline("Miners sell into strength", "", &[" Bitcoin "])));
    assert!(!btc.is_named_in(&headline("Miners sell into strength", "", &["Bitcoin Mining"])));
    assert!(follow("ONE", &[]).is_named_in(&headline("a one-day rally", "", &["ONE"])));
}

/// The catalog row's base, upper-cased, else the symbol itself; no words for a chart with no
/// symbol.
#[test]
fn the_words_are_the_catalog_rows_base_else_the_symbol() {
    let catalog = Catalog::from_instruments(vec![Instrument::new(
        "binance",
        "BTCUSDT",
        "BTC",
        "USDT",
        Contract::CryptoSpot,
    )]);
    assert_eq!(NewsFollow::of(&catalog, "binance", "BTCUSDT"), Some(follow("BTC", &[])));
    assert_eq!(NewsFollow::of(&catalog, "bybit", "BTCUSDT"), Some(follow("BTCUSDT", &[])));
    assert_eq!(NewsFollow::of(&catalog, "binance", "aapl"), Some(follow("AAPL", &[])));
    assert_eq!(NewsFollow::of(&catalog, "binance", ""), None);
}

/// The catalog is read when the chart or the catalog changes, never again for the same pair: an
/// answer from a list not loaded yet stays until the refreshed list arrives as a new `Arc`.
#[test]
fn the_words_are_read_once_per_chart_and_catalog() {
    let empty = Arc::new(Catalog::from_instruments(Vec::new()));
    let listed = Arc::new(Catalog::from_instruments(vec![Instrument::new(
        "binance",
        "BTCUSDT",
        "BTC",
        "USDT",
        Contract::CryptoSpot,
    )]));
    let btc = ("binance".to_string(), "BTCUSDT".to_string());
    let mut cache = NewsFollowCache::default();
    assert_eq!(cache.words(&empty, Some(&btc)).map(|w| w.ticker.clone()), Some("BTCUSDT".into()));
    assert_eq!(cache.words(&empty, Some(&btc)).map(|w| w.ticker.clone()), Some("BTCUSDT".into()));
    assert_eq!(cache.words(&listed, Some(&btc)).map(|w| w.ticker.clone()), Some("BTC".into()));
    let eth = ("binance".to_string(), "ETHUSDT".to_string());
    assert_eq!(cache.words(&listed, Some(&eth)).map(|w| w.ticker.clone()), Some("ETHUSDT".into()));
    assert_eq!(cache.words(&listed, None), None, "no chart: nothing to follow");
}
