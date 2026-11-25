use super::*;
use crate::refusals::DeribitHistoryRefusalId;
use vike_marketdata::OptionKind;

fn page_of(body: &str, currency: &str) -> Result<OptionTradesPage, DeribitHistoryRefusal> {
    parse_option_trades_page(currency, &serde_json::from_str(body).unwrap())
}

/// A recorded page (BTC options, 2024-06-03, `count=3`): every field the row keeps, the percent IV as a
/// decimal, the taker side as the aggressor flag, and the coin book's premium unit.
#[test]
fn a_recorded_page_maps_every_trade() {
    let page =
        page_of(include_str!("../tests/fixtures/deribit_option_trades_btc_page.json"), "BTC")
            .unwrap();
    assert!(page.has_more);
    assert_eq!(page.trades.len(), 3);
    let first = &page.trades[0];
    assert_eq!(first.ts, 1_717_372_800_864);
    assert_eq!(first.instrument, "BTC-21JUN24-94000-C");
    assert_eq!(first.underlying, "BTC");
    assert_eq!(first.expiry_ms, vike_options::expiry_ms("2024-06-21"));
    assert_eq!((first.strike, first.kind), (94_000.0, OptionKind::Call));
    assert_eq!(first.trade_id, "303871231");
    assert_eq!((first.price, first.amount), (0.0007, 2.7));
    assert!(first.is_buyer_maker, "direction `sell`: the taker sold");
    assert!((first.iv.unwrap() - 0.6337).abs() < 1e-12, "{:?}", first.iv);
    assert_eq!(first.mark_price, Some(0.000_784_82));
    assert_eq!(first.index_price, 67_735.8);
    assert!(first.premium_in_coin);
}

/// 2017's trades publish `mark_price: null`: the row keeps it absent, never zero.
#[test]
fn a_null_mark_stays_absent() {
    let page =
        page_of(include_str!("../tests/fixtures/deribit_option_trades_btc_2017.json"), "BTC")
            .unwrap();
    assert!(!page.has_more);
    let trade = &page.trades[0];
    assert_eq!(trade.mark_price, None);
    assert!((trade.iv.unwrap() - 0.2973).abs() < 1e-12, "{:?}", trade.iv);
    assert_eq!(
        (trade.instrument.as_str(), trade.strike, trade.kind),
        ("BTC-31MAR17-940-P", 940.0, OptionKind::Put)
    );
}

/// The USDC log mixes underlyings, prices in USDC, and writes a strike's fraction with a lower-case `d`; each
/// trade is named by its base, the live surface's name, until the listing says otherwise.
#[test]
fn the_usdc_log_is_linear_names_each_base_and_reads_a_decimal_strike() {
    let page =
        page_of(include_str!("../tests/fixtures/deribit_option_trades_usdc_page.json"), "USDC")
            .unwrap();
    let xrp = &page.trades[0];
    assert_eq!(xrp.instrument, "XRP_USDC-26SEP25-1d6-P");
    assert_eq!(xrp.underlying, "XRP");
    assert_eq!(xrp.strike, 1.6);
    assert!(!xrp.premium_in_coin);
    assert!(!xrp.is_buyer_maker, "direction `buy`");
    assert_eq!(page.trades[1].underlying, "SOL");
}

/// The chain name is the live surface's, by its listing: SOL's USDC book is `SOL` (SOL is served from it),
/// XRP is not listed and keeps its base, BTC's USDC book is `BTC_USDC` (BTC is served from its coin book, so
/// its linear options never mix into the `BTC` chain), and a coin-settled trade keeps its base whatever the
/// listing says.
#[test]
fn a_linear_book_off_the_live_surface_keeps_its_own_name() {
    let listed = [
        OptionUnderlying { name: "BTC".into(), book: "BTC".into(), index: "btc_usd".into() },
        OptionUnderlying { name: "SOL".into(), book: "USDC".into(), index: "sol_usdc".into() },
    ];
    let named = |instrument: &str, base: &str, coin: bool, currency: &str| {
        let mut row = trade(1, "x");
        row.instrument = instrument.into();
        row.underlying = base.into();
        row.premium_in_coin = coin;
        let mut rows = [row];
        name_off_surface_books(&mut rows, currency, &listed);
        rows[0].underlying.clone()
    };
    assert_eq!(named("SOL_USDC-27JUN25-160-C", "SOL", false, "USDC"), "SOL");
    assert_eq!(named("XRP_USDC-26SEP25-1d6-P", "XRP", false, "USDC"), "XRP");
    assert_eq!(named("BTC_USDC-6NOV26-77000-C", "BTC", false, "USDC"), "BTC_USDC");
    assert_eq!(named("BTC-21JUN24-94000-C", "BTC", true, "BTC"), "BTC");
}

/// The venue's error envelope, and a trade a row cannot be made of, are errors: a short page would be stored.
#[test]
fn an_error_envelope_or_a_bad_trade_fails_the_page() {
    let err = page_of(
        include_str!("../tests/fixtures/deribit_option_trades_invalid_currency.json"),
        "XYZ",
    )
    .unwrap_err();
    assert_eq!(
        err,
        DeribitHistoryRefusal::VenueErrorWithReason {
            what: "deribit option trades",
            code: -32602,
            msg: "Invalid params".to_string(),
            reason: "invalid currency".to_string(),
        }
    );
    let bad_trade = |body: &str| match page_of(body, "BTC").unwrap_err() {
        DeribitHistoryRefusal::OptionTradesBadTrade { currency, why } if currency == "BTC" => *why,
        other => panic!("not a bad trade: {other}"),
    };
    let bad = r#"{"result":{"has_more":false,"trades":[{"trade_id":"1","timestamp":1,"price":1,"amount":1,
        "direction":"buy","index_price":1,"instrument_name":"BTC-PERPETUAL"}]}}"#;
    assert_eq!(
        bad_trade(bad),
        DeribitHistoryRefusal::NotAnOptionName { instrument: "BTC-PERPETUAL".to_string() }
    );
    let no_index = r#"{"result":{"has_more":false,"trades":[{"trade_id":"1","timestamp":1,"price":1,"amount":1,
        "direction":"buy","instrument_name":"BTC-21JUN24-94000-C"}]}}"#;
    assert!(
        matches!(bad_trade(no_index), DeribitHistoryRefusal::TradeWithoutNumber { key, .. } if key == "index_price")
    );
    assert_eq!(
        page_of(r#"{"result":{"trades":[]}}"#, "BTC").unwrap_err(),
        DeribitHistoryRefusal::OptionTradesMissing { what: "result.has_more" }
    );
}

fn trade(ts: i64, id: &str) -> OptionTradeRow {
    let mut row =
        page_of(include_str!("../tests/fixtures/deribit_option_trades_btc_page.json"), "BTC")
            .unwrap()
            .trades
            .remove(0);
    row.ts = ts;
    row.trade_id = id.into();
    row
}

/// The walk resumes AT the last trade's millisecond and skips by id what it already took there, so two
/// trades of one millisecond split across pages are both kept, once each; trades outside the window go.
#[test]
fn the_walk_resumes_at_the_last_millisecond_and_keeps_each_trade_once() {
    let mut asked = Vec::new();
    let pages = |cursor: i64| -> Result<OptionTradesPage, String> {
        Ok(match cursor {
            10 => OptionTradesPage {
                trades: vec![trade(10, "a"), trade(12, "b"), trade(15, "c")],
                has_more: true,
            },
            // the venue repeats `c` (same millisecond) and adds `d` of that millisecond
            15 => OptionTradesPage {
                trades: vec![trade(15, "c"), trade(15, "d"), trade(20, "e"), trade(99, "late")],
                has_more: false,
            },
            other => panic!("asked from {other}"),
        })
    };
    let rows = walk_option_trades(10, 30, |cursor| {
        asked.push(cursor);
        pages(cursor)
    })
    .unwrap();
    let ids: Vec<&str> = rows.iter().map(|t| t.trade_id.as_str()).collect();
    assert_eq!(ids, ["a", "b", "c", "d", "e"]);
    assert_eq!(asked, [10, 15]);
}

/// A full page that cannot move, and an empty page that says it has more, fail the walk by name.
#[test]
fn a_walk_that_cannot_move_fails() {
    let stuck = walk_option_trades(10, 30, |_| {
        Ok(OptionTradesPage { trades: vec![trade(10, "a"), trade(10, "b")], has_more: true })
    })
    .unwrap_err();
    assert_eq!(stuck.id(), DeribitHistoryRefusalId::OptionTradesPageStuck, "{stuck}");
    let empty =
        walk_option_trades(10, 30, |_| Ok(OptionTradesPage { trades: vec![], has_more: true }))
            .unwrap_err();
    assert_eq!(empty.id(), DeribitHistoryRefusalId::OptionTradesEmptyPage, "{empty}");
    assert!(
        walk_option_trades(10, 30, |_| Ok(OptionTradesPage { trades: vec![], has_more: false }))
            .unwrap()
            .is_empty()
    );
}

/// The source dials the history host's own row, and a currency that is not a venue word is refused
/// before anything is sent.
#[test]
fn the_source_reads_the_trades_row_and_refuses_a_bad_currency_unsent() {
    let seeded = VenueUrls::seed(VENUE);
    let source = OptionTradeSourceDeribit::from_urls(&seeded).unwrap();
    assert_eq!(source.base, "https://history.deribit.com");
    assert_eq!(
        source.listing_url,
        "https://www.deribit.com/api/v2/public/get_instruments?currency=any&kind=option"
    );
    assert!(OptionTradeSourceDeribit::from_urls(&VenueUrls::default()).is_err());
    for currency in ["", "btc", "BTC&kind=future"] {
        let err = source.fetch(currency, 0, 1).unwrap_err();
        let refusal = DeribitHistoryRefusal::NotACurrencyWord { currency: currency.to_string() };
        assert_eq!(err, SourceError::Refused(refusal.carried()));
    }
}

/// Live, one hour of BTC option trades from the history host: pages that resume at a shared millisecond
/// keep every trade once (`cargo test -p vike-deribit --lib option_trades -- --ignored`).
#[test]
#[ignore = "network: reads history.deribit.com"]
fn live_one_hour_of_btc_option_trades() {
    let source = OptionTradeSourceDeribit::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    // 2024-06-03 00:00..01:00 UTC
    let rows = source.fetch("BTC", 1_717_372_800_000, 1_717_376_399_999).unwrap();
    let ids: HashSet<&str> = rows.iter().map(|t| t.trade_id.as_str()).collect();
    assert_eq!(ids.len(), rows.len());
    assert!(rows.windows(2).all(|w| w[0].ts <= w[1].ts));
    eprintln!("live: {} BTC option trades in the hour", rows.len());
}

/// Live: is the venue's `start_timestamp` inclusive? Two raw pages of 2024-06-03 (BTC, 14,251 trades in
/// two pages), the second asked from the first's last millisecond: the walk's de-duplication is what keeps
/// a shared millisecond whole either way, and this prints which way the venue went.
#[test]
#[ignore = "network: reads history.deribit.com"]
fn live_the_second_page_starts_at_the_first_pages_last_millisecond() {
    let base = "https://history.deribit.com";
    let (start, end) = (1_717_372_800_000, 1_717_459_199_999);
    let agent = blocking_agent();
    let page = |cursor: i64| {
        json_page_fetch(&agent, &trades_url(base, "BTC", cursor, end), &TRADES_PAGE, |v| {
            parse_option_trades_page("BTC", v).map_err(|e| e.to_string())
        })
        .unwrap()
    };
    let first = page(start);
    assert!(first.has_more);
    let last = first.trades.last().unwrap().clone();
    let second = page(last.ts);
    let overlap = second.trades.iter().take_while(|t| t.ts == last.ts).count();
    let repeats = second.trades.iter().any(|t| t.trade_id == last.trade_id);
    eprintln!(
        "live: page 2 from {} holds {overlap} trades of that millisecond; repeats page 1's last trade: {repeats}",
        last.ts
    );
    let day = fetch_option_trades_range(base, "BTC", start, end).unwrap();
    let ids: HashSet<&str> = day.iter().map(|t| t.trade_id.as_str()).collect();
    assert_eq!(ids.len(), day.len());
    eprintln!("live: the walk keeps {} BTC option trades of 2024-06-03", day.len());
}

/// Live: the sample the size and time estimate of a long backfill is made from — BTC and ETH, one day in
/// each of the four years the book reads, each day's trades and fetch seconds written as one JSON line to
/// `vike-option-trades-sample.jsonl` in the workspace's gitignored `tmp/` (never the shared system temp
/// folder: `temp_path_gate`), where `vike-backfill`'s
/// `the_live_sample_in_parquet` reads them (`cargo test -p vike-deribit --lib live_sample -- --ignored`).
#[test]
#[ignore = "network: reads history.deribit.com"]
fn live_sample_days_for_the_size_estimate() {
    use std::io::Write;
    let source = OptionTradeSourceDeribit::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    // 2023-03-01, 2024-03-01, 2025-03-03, 2026-03-02 (UTC midnights)
    let days = [1_677_628_800_000_i64, 1_709_251_200_000, 1_740_960_000_000, 1_772_409_600_000];
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../tmp/vike-option-trades-sample.jsonl");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut out = std::fs::File::create(&path).unwrap();
    for currency in ["BTC", "ETH"] {
        for d0 in days {
            let started = Instant::now();
            let rows = source.fetch(currency, d0, d0 + 86_399_999).unwrap();
            let seconds = started.elapsed().as_secs_f64();
            eprintln!("live: {currency} {d0}: {} trades in {seconds:.1}s", rows.len());
            let line = serde_json::json!({ "currency": currency, "day_ms": d0, "seconds": seconds, "rows": rows });
            writeln!(out, "{line}").unwrap();
        }
    }
    eprintln!("live: the sample is {}", path.display());
}

/// Live: a day of the USDC log is named like the live surface — SOL's book is `SOL`, never `SOL_USDC`, and
/// the books of underlyings the surface serves from their coin book keep their own head
/// (`cargo test -p vike-deribit --lib live_usdc -- --ignored`).
#[test]
#[ignore = "network: reads history.deribit.com and the www listing"]
fn live_usdc_trades_are_named_like_the_live_surface() {
    let source = OptionTradeSourceDeribit::from_urls(&VenueUrls::seed(VENUE)).unwrap();
    // 2025-06-02 (UTC)
    let rows = source.fetch("USDC", 1_748_822_400_000, 1_748_908_799_999).unwrap();
    let names: std::collections::BTreeSet<&str> =
        rows.iter().map(|t| t.underlying.as_str()).collect();
    eprintln!("live: {} USDC option trades, chains {names:?}", rows.len());
    assert!(names.contains("SOL") && !names.contains("SOL_USDC"), "{names:?}");
    assert!(
        !names.contains("BTC") && !names.contains("ETH"),
        "a linear BTC/ETH trade mixed into a coin chain: {names:?}"
    );
}
