//! The mount's one informational line per delayed stream (decision 0140), and the quote sides a
//! closed bar carries to the paper book (decision 0285).
use super::*;
use vike_execution::{ExecutionClient, Ingest, TickSender};
use vike_model::OrderRequest;

/// One line per not-real-time stream, naming the symbol; a repeat, a flip back to real-time and a
/// real-time stream log nothing; another stream of the same symbol is its own stream.
#[tracing_test::traced_test]
#[test]
fn a_delayed_stream_is_announced_once_and_a_real_time_one_never() {
    let a = DelayAnnouncer::default();
    assert!(!a.note("ibkr", "EUR.USD.IDEALPRO", "quotes", StreamDelay::RealTime));
    assert!(a.note("ibkr", "AAPL.SMART.USD", "quotes", StreamDelay::DELAYED));
    assert!(!a.note("ibkr", "AAPL.SMART.USD", "quotes", StreamDelay::DELAYED));
    assert!(!a.note("ibkr", "AAPL.SMART.USD", "quotes", StreamDelay::RealTime));
    assert!(!a.note("ibkr", "AAPL.SMART.USD", "quotes", StreamDelay::Frozen));
    assert!(a.note("ibkr", "AAPL.SMART.USD", "1m", StreamDelay::Frozen));
    assert!(logs_contain("ibkr AAPL.SMART.USD quotes: market data is delayed"));
    assert!(logs_contain("ibkr AAPL.SMART.USD 1m: market data is frozen"));
    assert!(!logs_contain("EUR.USD.IDEALPRO"), "a real-time stream is never announced");
}

// ---- the quote sides a closed bar carries (decision 0285) ----

/// `[open, high, low, close]` of a side path, as bits.
fn ohlc(b: &Bar) -> [u64; 4] {
    [b.open, b.high, b.low, b.close].map(f64::to_bits)
}

fn bits(path: [f64; 4]) -> [u64; 4] {
    path.map(f64::to_bits)
}

/// A window's path opens at the quote standing when it began; a window no quote moved is that quote
/// alone; an older window is unknown; a quote with a side that is no price is skipped; and a series
/// whose day starts at 21:00 UTC is cut there, then re-cut when it moves (daylight saving).
#[test]
fn quote_side_paths_open_at_the_standing_quote_and_cut_where_the_series_does() {
    let mut p = QuoteSidePaths::new(60_000, 0);
    assert!(p.sides(0, 0).is_none(), "no quote yet");
    p.on_quote(1_000, 10.0, 11.0);
    p.on_quote(2_000, 9.0, 10.0);
    p.on_quote(3_000, 0.0, 12.0); // one side is no price: skipped
    p.on_quote(61_000, 12.0, 13.0); // the next window, opened at 9 / 10
    let w0 = p.sides(0, 0).expect("window 0 closed");
    assert_eq!(ohlc(&w0.bid), bits([10.0, 10.0, 9.0, 9.0]));
    assert_eq!(ohlc(&w0.ask), bits([11.0, 11.0, 10.0, 10.0]));
    let w1 = p.sides(60_000, 60_000).expect("window 1 folding");
    assert_eq!(ohlc(&w1.bid), bits([9.0, 12.0, 9.0, 12.0]));
    assert_eq!(ohlc(&w1.ask), bits([10.0, 13.0, 10.0, 13.0]));
    let quiet = p.sides(180_000, 180_000).expect("no quote moved window 3: 12 / 13 stood");
    assert_eq!(ohlc(&quiet.ask), bits([13.0; 4]));
    p.on_quote(250_000, 14.0, 15.0); // window 4: window 1 closes, windows 2 and 3 stood at 12 / 13
    assert_eq!(ohlc(&p.sides(120_000, 120_000).unwrap().bid), bits([12.0; 4]));
    assert!(p.sides(0, 0).is_none(), "older than the last closed window: unknown");

    let (hour, day) = (3_600_000, 86_400_000);
    let mut d = QuoteSidePaths::new(day, 21 * hour);
    d.on_quote(20 * hour, 1.0, 2.0); // the day that began at 21:00 the evening before
    d.on_quote(22 * hour, 3.0, 4.0); // a new day
    assert_eq!(d.sides(21 * hour - day, 0).unwrap().ask.close, 2.0);
    d.realign(22 * hour); // the venue's day now starts an hour later: what was folded goes
    assert!(d.sides(22 * hour, 0).is_none());
}

/// The sink a venue's feed arm builds, over a channel this test reads: the series is seeded at 0
/// (its 1m windows cut there), `quotes` arrive, then the closed bar of window 60 000.
fn closed_bar_after(venue: &str, symbol: &str, quotes: &[(i64, f64, f64)], bar: Bar) -> BarUpdate {
    let (bars, market, mut rx) = vike_execution::market_data_channel(64);
    let ticks = TickSender { ingest: bars.ingest.clone() };
    let sink = CoreLaneSink::new(bars, market, ticks);
    sink.seed_bars(venue, symbol, "1m", vec![Bar::ohlcv(0, 1.0, 1.0, 1.0, 1.0, 1.0)]);
    for &(ts, bid, ask) in quotes {
        let quote = TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 1.0,
            ask_size: 1.0,
            symbol: symbol.into(),
        };
        sink.quote(venue, symbol, quote);
    }
    sink.close_bar(venue, symbol, "1m", bar);
    loop {
        if let Ingest::BarClose(update) = rx.try_recv().expect("the bar reached the bar lane") {
            return *update;
        }
    }
}

/// A paper market order of `side` on `(venue, symbol)`, filled on `update`: its price, or the
/// book's refusal.
fn paper_market(venue: &str, symbol: &str, side: i32, update: &BarUpdate) -> Result<f64, String> {
    paper_market_stated(venue, symbol, side, update, None)
}

/// [`paper_market`] on a book the engine told `symbol`'s contract value, when `stated` says one
/// (`ExecutionClient::on_contract_value`).
fn paper_market_stated(
    venue: &str,
    symbol: &str,
    side: i32,
    update: &BarUpdate,
    stated: Option<vike_model::ContractValue>,
) -> Result<f64, String> {
    let mut book = vike_paper::ExecutionClientPaper::new(venue, symbol, 0.0, 0.0, 0.0);
    if let Some(value) = stated {
        book.on_contract_value(symbol, value);
    }
    book.submit(&OrderRequest {
        order_client_id: "m".into(),
        venue: venue.into(),
        symbol: symbol.into(),
        side,
        qty: 1.0,
        order_type: vike_model::OrderType::Market,
        ..Default::default()
    });
    let events: Vec<_> = std::iter::from_fn(|| book.poll_events()).collect();
    if let Some(why) = vike_execution::testing::rejection(&events) {
        return Err(why);
    }
    book.on_bar_sides(&update.bar, update.sides.as_deref());
    let fills = book.fills.lock().unwrap().clone();
    Ok(fills.first().expect("a market order fills on the next bar").price)
}

/// The owner's rule (2026-11-16) on a venue whose candles are TRADES or MIDS: the bar opens at
/// `print`, the venue's quote stands at `bid` / `ask`, and a paper market buy pays the ask and a sell
/// gets the bid, never the print (what it paid before).
fn crosses_the_spread(venue: &str, symbol: &str, bid: f64, ask: f64, print: f64) {
    let bar = Bar::ohlcv(60_000, print, print, print, print, 1.0);
    let update = closed_bar_after(venue, symbol, &[(61_000, bid, ask)], bar);
    assert!(update.sides.is_some(), "{venue}: the closed bar carries its quote sides");
    assert_eq!(paper_market(venue, symbol, 1, &update), Ok(ask), "{venue}: a buy pays the ask");
    assert_eq!(paper_market(venue, symbol, -1, &update), Ok(bid), "{venue}: a sell gets the bid");
    let bare = BarUpdate { sides: None, ..update };
    assert_eq!(paper_market(venue, symbol, 1, &bare), Ok(print), "{venue}: the bar alone");
}

#[test]
fn a_binance_paper_market_order_crosses_the_spread() {
    crosses_the_spread("binance", "BTCUSDT", 99_990.0, 100_010.0, 100_000.0);
}

#[test]
fn a_bybit_paper_market_order_crosses_the_spread() {
    crosses_the_spread("bybit", "BTCUSDT", 99_990.0, 100_010.0, 100_000.0);
}

#[test]
fn an_okx_paper_market_order_crosses_the_spread() {
    crosses_the_spread("okx", "BTC-USDT-SWAP", 99_990.0, 100_010.0, 100_000.0);
}

#[test]
fn an_aster_paper_market_order_crosses_the_spread() {
    crosses_the_spread("aster", "BTCUSDT.P", 99_990.0, 100_010.0, 100_000.0);
}

#[test]
fn a_hyperliquid_paper_market_order_crosses_the_spread() {
    crosses_the_spread("hyperliquid", "BTC", 99_990.0, 100_010.0, 100_000.0);
}

#[test]
fn a_deribit_paper_market_order_crosses_the_spread() {
    crosses_the_spread("deribit", "BTC_USDC-PERPETUAL", 99_990.0, 100_010.0, 100_000.0);
}

#[test]
fn an_alpaca_paper_market_order_crosses_the_spread() {
    crosses_the_spread("alpaca", "AAPL", 229.98, 230.02, 230.0);
}

/// OANDA's candles are MIDS (`price=M`): 1.08500 with its quote at 1.08495 / 1.08505.
#[test]
fn an_oanda_paper_market_order_crosses_the_spread() {
    crosses_the_spread("oanda", "EUR_USD", 1.08495, 1.08505, 1.08500);
}

/// IG's candles are MIDS too, and its closed candle carries the sides of IG's own quote. A paper book
/// refuses an IG epic until the engine states its contract size (read over ig's demo login, the
/// owner's ruling of 2026-11-16), in his words, and then crosses the spread like every venue.
#[test]
fn an_ig_candle_carries_its_quote_sides_and_the_paper_book_fills_the_epic_once_its_size_is_stated()
{
    let epic = "CS.D.EURUSD.MINI.IP";
    let bar = Bar::ohlcv(60_000, 1.08500, 1.08500, 1.08500, 1.08500, 1.0);
    let update = closed_bar_after("ig", epic, &[(61_000, 1.08495, 1.08505)], bar);
    let sides = update.sides.as_deref().expect("the IG candle carries its quote sides");
    assert_eq!((sides.bid.open, sides.ask.open), (1.08495, 1.08505));
    let refused = paper_market("ig", epic, 1, &update).expect_err("an unstated IG epic is refused");
    assert!(refused.starts_with("no price: no demo login for ig"), "{refused}");
    let stated = Some(vike_model::ContractValue::linear(10_000.0));
    for (side, price) in [(1, 1.08505), (-1, 1.08495)] {
        assert_eq!(paper_market_stated("ig", epic, side, &update, stated), Ok(price), "{side}");
    }
}

/// No quote reached the series, or the interval has no fixed width: the bar closes with no sides and
/// fills as it always did; nothing is guessed.
#[test]
fn a_bar_with_no_quote_or_no_fixed_width_closes_with_no_sides() {
    let bar = Bar::ohlcv(60_000, 100.0, 100.0, 100.0, 100.0, 1.0);
    assert!(closed_bar_after("binance", "BTCUSDT", &[], bar.clone()).sides.is_none());
    let (bars, market, mut rx) = vike_execution::market_data_channel(64);
    let ticks = TickSender { ingest: bars.ingest.clone() };
    let sink = CoreLaneSink::new(bars, market, ticks);
    sink.seed_bars("binance", "BTCUSDT", "1w", vec![bar.clone()]);
    let quote = TickQuote {
        timestamp: 61_000,
        local_timestamp: 0,
        bid: 99.0,
        ask: 101.0,
        bid_size: 1.0,
        ask_size: 1.0,
        symbol: Default::default(),
    };
    sink.quote("binance", "BTCUSDT", quote);
    sink.close_bar("binance", "BTCUSDT", "1w", bar);
    let update = std::iter::from_fn(|| rx.try_recv().ok())
        .find_map(|i| match i {
            Ingest::BarClose(u) => Some(u),
            _ => None,
        })
        .expect("the bar reached the bar lane");
    assert!(update.sides.is_none(), "a week has no fixed width: no sides");
}
