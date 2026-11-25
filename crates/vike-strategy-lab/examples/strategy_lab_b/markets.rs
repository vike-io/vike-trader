//! The market of a row: the venue and its series the bars are of, the length of one bar, and the symbol's own trading rules,
//! which two of `main.rs`'s settled rules depend on (a size worked out from money floors to the quantity step; slippage and the
//! stop and limit grid are counted in ticks). A row's market is its reference's `symbol` (`BINANCE:ETHUSDT`,
//! `BYBIT:WIFUSDT.P`: the venue, then its name of the series, the datahub's too) and `timeframe`; `main.rs` sets it before
//! every run (`set`) and the rules that need it read it (`current`).
//!
//! # The trading rules are the venue's instrument facts, never a table here
//!
//! The quantity step and the tick are the symbol's KEPT instrument row, the venue's own list the datahub keeps
//! (`<project>/instruments/<venue>.json`), read through the datahub once per process as a backtest reads it
//! (`vike_data::HistoryStore::kept_instruments`, `vike_catalog::kept_row`): its `step_size` (the venue's `LOT_SIZE` step on
//! Binance spot, which is also its floor: a size under one step is no order) and its `tick_size`. A market the kept lists
//! carry no row of is no market here: its reference rows are not read. Every symbol set B has rows on has a step and a tick
//! of a power of ten, each kept as `1 / 10^n` (`power_of_ten`), so a number carried through the wire is the same bits as
//! that power written out. BTCUSDT's step (0.00001) is what set B's first ten rows measured (`main.rs`'s rule 3); the
//! others are each confirmed by the rows of that symbol (the `unrounded sizes` control and the matching nets, which `main.rs`'s
//! `//!` counts).
//!
//! **Unsettled, and no `PLANS` row depends on it:** on the two rows that size from money in a coin with a coarse step, the
//! venue's step is not the closest reading. `hulk_grid_algorithm_v2_the_quant_science` (LTCUSDT) is 2.77 off in the net with the
//! step 0.001 and 0.074 off with 0.01 (the trades match either way; neither is within 0.01), and a replay of `ik_grid_script`
//! (DOGEUSDT) gets the nearest nets with 0.1, not the venue's 1. The run keeps the venue's numbers until a row matches.
//!
//! # A reference market no venue carries (`STAND_INS`)
//!
//! A reference ran on a broker's CFD feed the roster has no venue for (`CAPITALCOM:EURUSD`) runs on a venue's series of the same
//! pair instead, Dukascopy's EURUSD, a forex feed: its prices are another feed's, so a row there is checked, never assumed, to
//! match. That venue holds the pair at finer bar lengths only (1 and 15 minutes), so every bar length a row reads is built from
//! its 15-minute bars (`timeframe::built`). Its kept row states neither a step nor a tick, so the stand-in states the
//! reference market's own: the tick its scripts count their levels in (`syminfo.mintick`, 0.00001), and a step of one unit.
//! The pair trades the FX week, so its bars stop over a weekend and a holiday (`Market::closes`).
//!
//! A reference that ran on a US exchange's share (`NASDAQ:AAPL`, `AMEX:SPY`) runs on the venue's CFD of it, Dukascopy's
//! `AAPL.CFD` and `SPY.CFD`: a CFD, not the exchange stock, quoted in the regular session only, its bars of each length (the
//! daily ones too) built by the import from the CFD's own ticks. Its rules are the reference market's: the tick of a share
//! (0.01) and a step of one share. Neither held series is split-adjusted (AAPL's 4:1 split shows as 499.19 on 2020-08-28 and
//! 127.64 on 2020-08-31), where the reference's is; the lab reads AAPL's adjusted by the operator's split row (decision 0289,
//! set A's `bars.rs`). A stand-in's series are the operator's imports of the reference windows, read
//! from their first bar (`is_stand_in`).
//!
//! The held 15-minute bars are MID prices. A row listed in a stand-in's `bid_rows` runs on bars built from the BID side of
//! the venue's quote ticks instead (`main.rs`' `bid_quarter_hours`), the side that came closer to its reference, the trade
//! count first, then the net (IMP-I7a, 2026-11-16: bid and mid measured on each of the five rows, none matching on either).

use std::cell::Cell;
use std::sync::OnceLock;

use vike_model::Instrument;

/// One symbol's trading rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Market {
    /// The roster's venue whose candles and instrument row these are.
    pub venue: &'static str,
    /// The series' name here, in the lab's store and the run's profile: the venue's (`series`), or a stand-in's own for
    /// another price side of it (`StandIn::bid`).
    pub symbol: &'static str,
    /// The venue's name of the series its bars are read from, the datahub's too.
    pub series: &'static str,
    /// Decimals of the quantity step: 5 is 0.00001.
    pub step_decimals: usize,
    pub tick: f64,
    /// The venue's bar length every bar length of this market is built from (`timeframe::built`), for a series the venue
    /// holds at no other; `None`: each is the venue's own.
    pub built_from: Option<&'static str>,
    /// The market has closed time (the FX week: weekends and holidays), so its bars, and every input of it, stop there.
    pub closes: bool,
}

/// `STAND_INS`' row: the reference's market, the venue's series run in its place, and the reference market's trading rules.
struct StandIn {
    reference: &'static str,
    market: Market,
    /// The same market on the bid side of the venue's ticks, and the rows that run on it (the module doc).
    bid: Market,
    bid_rows: &'static [&'static str],
}

/// The stand-in EURUSD: the venue, its series, the reference market's tick and a step of one unit.
const EURUSD: Market = Market {
    venue: "dukascopy",
    symbol: "EURUSD",
    series: "EURUSD",
    step_decimals: 0,
    tick: 0.00001,
    built_from: Some("15m"),
    closes: true,
};

/// A US share's CFD on Dukascopy, `series` its name there, at the reference market's tick and a step of one share.
const fn share_cfd(series: &'static str) -> Market {
    Market {
        venue: "dukascopy",
        symbol: series,
        series,
        step_decimals: 0,
        tick: 0.01,
        built_from: None,
        closes: true,
    }
}

/// The reference markets no venue on the roster carries, each run on a venue's series of the same instrument (the module doc).
const STAND_INS: [StandIn; 3] = [
    StandIn {
        reference: "CAPITALCOM:EURUSD",
        market: EURUSD,
        bid: Market { symbol: "EURUSD_BID", built_from: Some(BID), ..EURUSD },
        bid_rows: &["lanz_strategy_2_0_backtest"],
    },
    StandIn {
        reference: "NASDAQ:AAPL",
        market: share_cfd("AAPL.CFD"),
        bid: share_cfd("AAPL.CFD"),
        bid_rows: &[],
    },
    StandIn {
        reference: "AMEX:SPY",
        market: share_cfd("SPY.CFD"),
        bid: share_cfd("SPY.CFD"),
        bid_rows: &[],
    },
];

/// Whether `market` is a stand-in's (`STAND_INS`), whose series are read from their first bar (the module doc).
pub fn is_stand_in(market: Market) -> bool {
    STAND_INS.iter().any(|s| s.market == market || s.bid == market)
}

/// `Market::built_from` for bars built from the BID side of the venue's quote ticks (`main.rs`' `bid_quarter_hours`).
pub const BID: &str = "bid";

/// `chart` as reference row `row` runs on it: a stand-in's bid-side market for a row of its `bid_rows`, else `chart` itself.
pub fn on_row(row: &str, chart: Chart) -> Chart {
    match STAND_INS.iter().find(|s| s.market == chart.market && s.bid_rows.contains(&row)) {
        Some(stand_in) => Chart { market: stand_in.bid, ..chart },
        None => chart,
    }
}

/// A market at one bar length: what a row's bars are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chart {
    pub market: Market,
    /// The venue's interval name, the datahub's too: `30m`, `4h`, `1d`.
    pub interval: &'static str,
    pub bar_ms: i64,
}

/// Every venue's kept instrument rows, read through the datahub once.
fn kept_rows() -> &'static [Instrument] {
    static ROWS: OnceLock<Vec<Instrument>> = OnceLock::new();
    ROWS.get_or_init(|| {
        let rows = crate::bars::datahub()
            .kept_instruments()
            .unwrap_or_else(|e| panic!("the datahub's kept instrument lists: {e}"))
            .expect("the datahub serves its kept instrument lists");
        assert!(!rows.is_empty(), "the datahub keeps no instrument list");
        rows
    })
}

/// `n` when `x` is `1 / 10^n` (n of 0 to 12), to a part in a billion: what a step or a tick of a power of ten is kept as.
fn power_of_ten(x: f64) -> Option<usize> {
    (0..=12).find(|&n| (x * 10f64.powi(n as i32) - 1.0).abs() < 1e-9)
}

/// The market a reference's `symbol` names (`BINANCE:ETHUSDT`), from its venue's kept row; `None` for a venue off the roster or
/// a series its list does not carry.
fn market_of(symbol: &str) -> Option<Market> {
    if let Some(stand_in) = STAND_INS.iter().find(|s| s.reference == symbol) {
        return Some(stand_in.market);
    }
    let (venue, name) = symbol.split_once(':')?;
    let venue = *vike_model::VENUES.iter().find(|v| v.eq_ignore_ascii_case(venue))?;
    let row = vike_catalog::kept_row(kept_rows(), venue, name)?;
    let step = row.properties.step_size;
    let step_decimals = power_of_ten(step).unwrap_or_else(|| {
        panic!(
            "{symbol}: the quantity step {step} is not a power of ten, which `sizing.rs` floors to"
        )
    });
    let tick = row.properties.tick_size;
    assert!(tick > 0.0, "{symbol}: the kept row states no tick");
    let tick = power_of_ten(tick).map_or(tick, |n| 1.0 / 10f64.powi(n as i32));
    // shortcut: the name lives as long as the process (a market per reference row), which a `Copy` market needs.
    let symbol: &'static str = Box::leak(row.raw_symbol.clone().into_boxed_str());
    Some(Market {
        venue,
        symbol,
        series: symbol,
        step_decimals,
        tick,
        built_from: None,
        closes: false,
    })
}

/// The bar lengths this harness reads: the reference's name of each (its `timeframe`), the venue's interval name (the
/// datahub's too), and its length in ms.
const BAR_LENGTHS: [(&str, &str, i64); 8] = [
    ("1", "1m", 60_000),
    ("3", "3m", 180_000),
    ("5", "5m", 300_000),
    ("15", "15m", 900_000),
    ("30", "30m", 1_800_000),
    ("60", "1h", 3_600_000),
    ("240", "4h", 14_400_000),
    ("D", "1d", 86_400_000),
];

/// The chart a reference row names (`BINANCE:ETHUSDT`, `240`), `None` for a market or a bar length this harness has no bars of.
pub fn chart_of(symbol: &str, timeframe: &str) -> Option<Chart> {
    let &(_, interval, bar_ms) = BAR_LENGTHS.iter().find(|b| b.0 == timeframe)?;
    Some(Chart { market: market_of(symbol)?, interval, bar_ms })
}

/// The length in ms of the interval `name` (`4h`) this harness reads.
pub fn interval_ms(name: &str) -> i64 {
    BAR_LENGTHS.iter().find(|b| b.1 == name).map_or_else(|| panic!("no bars of {name}"), |b| b.2)
}

/// The interval `name` (`1d`) as this harness reads it, `None` for one it reads no bars of.
pub fn interval_named(name: &str) -> Option<&'static str> {
    BAR_LENGTHS.iter().map(|b| b.1).find(|&interval| interval == name)
}

thread_local! {
    static CURRENT: Cell<Option<Chart>> = const { Cell::new(None) };
}

/// The chart of the run in hand.
pub fn current() -> Chart {
    CURRENT
        .with(Cell::get)
        .expect("a run sets its market (`markets::set`) before anything reads it")
}

/// Makes `chart` the run in hand's, for `sizing.rs`, `timeframe.rs` and any rule that reads the tick.
pub fn set(chart: Chart) {
    CURRENT.with(|c| c.set(Some(chart)));
}

pub fn self_check() {
    assert_eq!(
        (power_of_ten(0.00001), power_of_ten(1.0), power_of_ten(0.5)),
        (Some(5), Some(0), None)
    );
    assert_eq!(power_of_ten(0.01).map(|n| 1.0 / 10f64.powi(n as i32)), Some(0.01));
    assert!(
        chart_of("BINANCE:BTCUSDT", "45").is_none() && chart_of("NOWHERE:BTCUSDT", "30").is_none()
    );
    let eurusd = chart_of("CAPITALCOM:EURUSD", "240").expect("a stand-in");
    assert_eq!(
        (eurusd.market.venue, eurusd.market.built_from, eurusd.bar_ms),
        ("dukascopy", Some("15m"), 14_400_000)
    );
    let bid = on_row("lanz_strategy_2_0_backtest", eurusd).market;
    assert_eq!((bid.symbol, bid.series, bid.built_from), ("EURUSD_BID", "EURUSD", Some(BID)));
    assert_eq!(on_row("fs_atr_ps_ma", eurusd), eurusd);
    let spy = chart_of("AMEX:SPY", "5").expect("a stand-in").market;
    assert!(is_stand_in(spy) && (spy.series, spy.step_decimals, spy.tick) == ("SPY.CFD", 0, 0.01));
}
