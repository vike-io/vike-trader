//! The market of a row: the Binance spot symbol its bars are of, the length of one bar, and the symbol's own trading rules, which
//! two of `main.rs`'s settled rules depend on (a size worked out from money floors to the quantity step; slippage and the stop
//! and limit grid are counted in ticks). A row's market is its reference's `symbol` and `timeframe`; `main.rs` sets it before
//! every run (`set`) and the rules that need it read it (`current`).
//!
//! # The per-symbol table (`MARKETS`), and where each number comes from
//!
//! | symbol | quantity step (= the quantity floor) | tick | minimum order value |
//! |---|---|---|---|
//! | BTCUSDT | 0.00001 | 0.01 | 5 USDT |
//! | ETHUSDT | 0.0001 | 0.01 | 5 USDT |
//! | DOGEUSDT | 1 | 0.00001 | 1 USDT |
//! | LTCUSDT | 0.001 | 0.01 | 5 USDT |
//!
//! Source of every number: the venue's public exchange info (`https://api.binance.com/api/v3/exchangeInfo?symbol=<SYMBOL>`,
//! read 2026-11-09): the step is the `LOT_SIZE` filter's `stepSize` (its `minQty` is the same number on all four, so the step is
//! also the floor: a size under one step is no order), the tick the `PRICE_FILTER`'s `tickSize`, the order value the `NOTIONAL`
//! filter's `minNotional`. BTCUSDT's step is what set B's first ten rows measured (`main.rs`'s rule 3); the others are each
//! confirmed by the rows of that symbol (the `unrounded sizes` control and the matching nets, which `main.rs`'s `//!` counts).
//! The reference applies no minimum order value on any row measured, so it is in the table for the record and read by nothing.
//!
//! **Unsettled, and no `PLANS` row depends on it:** on the two rows that size from money in a coin with a coarse step, the
//! venue's step is not the closest reading. `hulk_grid_algorithm_v2_the_quant_science` (LTCUSDT) is 2.77 off in the net with the
//! step 0.001 and 0.074 off with 0.01 (the trades match either way; neither is within 0.01), and a replay of `ik_grid_script`
//! (DOGEUSDT) gets the nearest nets with 0.1, not the 1 of the table. The table keeps the venue's numbers until a row matches.

use std::cell::Cell;

/// One symbol's trading rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Market {
    /// The venue's name, which is also the datahub's name for the spot pair's series.
    pub symbol: &'static str,
    /// Decimals of the quantity step: 5 is 0.00001.
    pub step_decimals: usize,
    pub tick: f64,
    /// The `NOTIONAL` filter's minimum order value, in USDT. Not applied by the reference on any row measured.
    pub min_notional: f64,
}

pub const BTCUSDT: Market =
    Market { symbol: "BTCUSDT", step_decimals: 5, tick: 0.01, min_notional: 5.0 };
pub const ETHUSDT: Market =
    Market { symbol: "ETHUSDT", step_decimals: 4, tick: 0.01, min_notional: 5.0 };
pub const DOGEUSDT: Market =
    Market { symbol: "DOGEUSDT", step_decimals: 0, tick: 0.00001, min_notional: 1.0 };
pub const LTCUSDT: Market =
    Market { symbol: "LTCUSDT", step_decimals: 3, tick: 0.01, min_notional: 5.0 };

const MARKETS: [Market; 4] = [BTCUSDT, ETHUSDT, DOGEUSDT, LTCUSDT];

/// A market at one bar length: what a row's bars are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chart {
    pub market: Market,
    /// The venue's interval name, the datahub's too: `30m`, `4h`, `1d`.
    pub interval: &'static str,
    pub bar_ms: i64,
}

impl Chart {
    /// Set B's first market, and what a run is on until `set` says otherwise.
    pub const BTC_30M: Chart = Chart { market: BTCUSDT, interval: "30m", bar_ms: 1_800_000 };
}

/// The chart a reference row names (`BINANCE:ETHUSDT`, `240`), `None` for a market this harness has no bars of.
pub fn chart_of(symbol: &str, timeframe: &str) -> Option<Chart> {
    let market = *MARKETS.iter().find(|m| symbol == format!("BINANCE:{}", m.symbol))?;
    let (interval, bar_ms) = match timeframe {
        "1" => ("1m", 60_000),
        "3" => ("3m", 180_000),
        "5" => ("5m", 300_000),
        "30" => ("30m", 1_800_000),
        "60" => ("1h", 3_600_000),
        "240" => ("4h", 14_400_000),
        "D" => ("1d", 86_400_000),
        _ => return None,
    };
    Some(Chart { market, interval, bar_ms })
}

thread_local! {
    static CURRENT: Cell<Chart> = const { Cell::new(Chart::BTC_30M) };
}

/// The chart of the run in hand.
pub fn current() -> Chart {
    CURRENT.with(Cell::get)
}

/// Makes `chart` the run in hand's, for `sizing.rs`, `timeframe.rs` and any rule that reads the tick.
pub fn set(chart: Chart) {
    CURRENT.with(|c| c.set(chart));
}

pub fn self_check() {
    let eth = chart_of("BINANCE:ETHUSDT", "240").expect("a listed market");
    assert_eq!((eth.market, eth.interval, eth.bar_ms), (ETHUSDT, "4h", 14_400_000));
    assert_eq!(chart_of("BINANCE:BTCUSDT", "30"), Some(Chart::BTC_30M));
    assert!(chart_of("AMEX:SPY", "5").is_none() && chart_of("BINANCE:BTCUSDT", "15").is_none());
    for m in MARKETS {
        let per_unit = (1.0 / m.tick).round();
        assert!(
            (per_unit * m.tick - 1.0).abs() < 1e-9,
            "{}: the tick is a whole fraction of 1",
            m.symbol
        );
    }
}
