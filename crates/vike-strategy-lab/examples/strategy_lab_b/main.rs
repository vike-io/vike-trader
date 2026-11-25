//! Set B: published strategies written by hand on vike, each checked against the reference, which is the trade count and the
//! net profit its script made over a window of a venue's bars, Binance spot BTCUSDT 30-minute ones for most rows (`markets.rs`
//! has the others). There are no trade lists, so a match is
//! both numbers: the count exactly and the net within `max(0.01, 1.2e-7 * |reference|)`, the reference being printed from
//! 32-bit floats. A row the owner accepted on numbers that miss its reference (`strategies::ACCEPTED`, its reason and his
//! dated ruling beside it) passes on exactly those numbers instead, to the bit, and on no other, its reference's own
//! included: the tolerance is never widened (decision 0021). A research harness, not product code: nothing in the workspace
//! names it.
//!
//! One `PLANS` row per strategy in `strategies/mod.rs`: the rule of its shipped strategy file by its name (`gallery!`, decision
//! 0269 W5), the four that look into the future included (decision 0271's amendment). Each runs as a `desk::Plan`: set A's `desk.rs` is the manual and is
//! shared here by path. A row's window is the
//! reference's, read from `reference_b.csv`, both ends bar open times and inclusive. Orders may be placed from the window's
//! first bar; one placed on its last bar never fills.
//!
//! # What the first ten settled, measured (window, then the alternative, against the reference)
//!
//! 1. **The reference saw no bar before the window.** Every row starts its indicators on the window's first bar. Fed from
//!    2024-12-01 instead, every row whose numbers move misses, 8 of the 10: for example trades 1069 / 602 / 797 / 64 / 1418
//!    against 1065 / 601 / 793 / 63 / 1417 (`evwma_vwap_macd_strategy_quantnomad`, `how_to_set_time_ranges`,
//!    `ichimoku_cloud`, `stochastic_trend`, `on_balance_volume_oscillator_strategy_quantnomad`), net 7221.96 against
//!    7296.04 (`evwma_vwap_cross_quantnomad`) and -25924.96 against -26270.45 (`support_resistance_strategy`). The
//!    control `warm-up from 2024-12-01` holds it.
//! 2. **A trade still open on the last bar is in neither number.** The count is the closed trades: 8 of the 10 rows end with
//!    one open and the reference counts it in none (681 closed, 1 open, reference 681 on
//!    `evwma_vwap_cross_quantnomad`). The net is the closed trades' alone, without even the open trade's entry
//!    commission: with that taken off, -329263.87 against -328993.875 (`how_to_set_time_ranges`, closed alone
//!    -328993.868) and -669.031 against -668.783 (`ichimoku_cloud`, closed alone -668.78294); with the open trade
//!    marked at the last close, 7836.02 against 7296.04. The three `open trade` controls hold it.
//! 3. **A size worked out from money floors to the symbol's quantity step, 0.00001 BTC** (`sizing.rs`). A share of the
//!    account's worth at the signal bar's close is `worth * share / (close * (1 + commission))`, a cash amount is
//!    `cash / close`, with no commission in it; both fill at the next open. Left unrounded, all four money-sized rows miss:
//!    1497.978 against 1497.894 (`ema_long_only_strategy`, floored 1497.894056), -32089.684 against -32089.486
//!    (`stochastic_trend`, floored -32089.48695), -669.020 against -668.783 (`ichimoku_cloud`) and -328995.554
//!    against -328993.875 (`how_to_set_time_ranges`). The control `unrounded sizes` holds it. **Commission** is the
//!    stated percent of each fill's value, on the entry and on the exit, and a reversal pays it once on its whole quantity,
//!    which is the same sum: 0.075% (793 trades), 0.2% (63) and 0.27% (601) all match. A wrong rate keeps the count and loses
//!    the net (the control `cost`).
//! 4. **A position the account cannot carry is margin-called, the reference's way** (`sizing::margin_call`, the engine's
//!    `vike_simulator::ShortfallCover`). Language version 6 states a margin, 100% of a position's value when its header gives
//!    none, and starts with 100,000 at 100% of equity when it gives neither size nor capital. A position short of margin
//!    at the bar's open or at its adverse extreme is cut there by four times its shortfall, floored to the step, or by one
//!    whole unit when that is under a step, each cut a trade of its own; an entry needing more margin than the account at
//!    its fill is refused, unless a close of the same bar went first. `adaptive_trend_flow_strategy_with_filters_for_sp`
//!    (20%) is 1779 / -986.911549 without the call and 1784 / -987.183838 with it, the reference to its last decimal;
//!    `gaussian_channel_strategy` (100%) is 1497 without, 2517 without the whole unit, 2527 with it;
//!    `crossing_moving_averages_with_adx_filter` (100%) is 300 without, 686 without the refusal, 685 with it.
//! 5. **A call for a higher timeframe without lookahead sees the last higher bar closed by this bar's close**, built
//!    UTC-aligned, its expression computed on the higher bars; a day, a week and a month reach back into the venue's own
//!    history before the window, a frame of minutes or hours and the Heikin-Ashi ticker start at the window, and the ticker's
//!    prices are on the tick (`timeframe.rs`'s `//!` has the measurements). With the bar in progress at its final values,
//!    all 8 rows that read a higher value miss (`previous_day_high_and_low_breakout_strategy` 296 trades against 214); with
//!    daily bars from the window alone, the rows that reading moves (net -16730.84 against -13897.51 on the same row); with
//!    4-hour bars from the venue's history, 240 trades against 225 (`mtf_screener_ma_cross_system`). The controls `higher
//!    bar in progress`, `higher bars from the window only`, `intraday higher bars and Heikin-Ashi candles from the venue's
//!    history` and `Heikin-Ashi candles off the tick` hold it.
//! 6. **The orders one stretch of a bar reaches fill in the order the price meets them, and each exit belongs to its entry**
//!    (`desk::Terms::price_order` and `desk::Terms::bound_exits`, on for every row in `run`; the reference's broker states both;
//!    a gallery row's file header states its own, `bound_exits = true` or more, which its run takes: `desk::Plan::proven`).
//!    At the open, what the open is past fills market orders first, then entries, then exits, each from the lowest level. An
//!    exit is sized from its own entry and dies with it while its fill closes the oldest trades whoever made them, the limit
//!    on entries counts the entries still bound, and an exit issued with an entry still to fill works from that fill on.
//!    `btfd_strategy_3min`, its exits guarding lots of one name, gave 1027 / -5378.607416; bound, 934 / -4809.907466 against
//!    the reference's 934 / -4809.908691. Every row of the set matches with both on; set A's list-b refutes each, 5 rows with
//!    the price order and 3 with bound exits (`desk.rs`'s `//!`).
//! 7. **A lower timeframe is the row's own symbol's bars of that length, from the bar the reference's platform served them
//!    from.** A call for a lower timeframe computes its expression on the lower bars and reads it at the last of them inside
//!    the chart bar: the rule reads an input of that length (decision 0271), whose bars closed by the chart bar's close. The
//!    platform serves a lower series only so far back (some 100,000 one-, three- or five-minute bars, a 15-minute one from the
//!    first of a month about fourteen months back), and the expression starts at that bar: the row's `Listed::inputs_from`
//!    (`strategies/mod.rs`) holds it, from the reference's own record of each series it read, and the run's store serves the
//!    series from it (`from_later_bars`; the desk's coverage tolerates the span before it). Read whole from 2024-12-01, the two
//!    rows measured first miss widely: 158 trades and 1253.829374 against 96 and 1121.570435
//!    (`multi_timeframe_rsi_buy_sell_strategy_tradedots`, 5 and 15 minutes), 225 and 612.110371 against 174 and -841.762573
//!    (`macd_ema_strategy`, 15 minutes); from those bars both match.
//!
//! Also measured: a script of language version 4 starts with 100,000 and one of version 5 with 1,000,000, read off the
//! reference's net as a percent of its start (7296.04 is 7.29604% on `evwma_vwap_cross_quantnomad`; 58296.20 is
//! 5.82962% on the version-5 `hhll`). Neither version refuses an entry the account cannot pay for: the
//! version-5 `how_to_set_time_ranges` keeps buying 100,000 worth with its account far below nothing (net -328994 on
//! 100,000), and the version-4 `rsi_strategy_with_alerts_via_tradingconnector_to` holds 100,000 BTC on 100,000.
//!
//! **A header that keeps the account in USD** (`Terms::account_usd`) on this USDT-quoted symbol books every amount in dollars
//! when it is booked, at the rate of its bar: Coinbase's USDT-USD daily close of the UTC day before the bar, read through the
//! datahub from the operator's own import of Coinbase's daily candles (`read_rates`, the lane `RATES_LANE`; Coinbase's terms
//! forbid passing them on, so they are never in the repository). A fill's commission and a closed
//! trade's profit at the fill's bar; a size from money divides by the placing bar's rate; the capital, a stated size and a size
//! the rule works out itself (`golden_triangle_strategy`'s `worth / close`) are not converted. 11 rows match on it. Booked in
//! USDT they miss, for example 14926.330707 against 14938.633789 (`pps`, 714 trades) and 2501 trades against 2500
//! (`hull_fisher`); the control `USD account booked in USDT` holds it. Six more rows that declare USD keep their count and
//! miss by 1.06 to 6.1 times the tolerance on this rule, likely a tick of the rate on a few days; they stay out, with their
//! numbers in the set's notes outside the repo. The rate is the backtest's own conversion pair (`vike_simulator::ConversionSeries`,
//! `desk::Terms::rate`), and on a day it has no rate (before its first day, 2021-05-04) one USDT is one dollar, the owner's
//! rulings of 2026-11-16 (decision 0243 amended; `vike_backtest::run::data_plan::converts_one_to_one_where_missing`): measured, it moves
//! none of the rows that match and adds `chandelier_exit_strategy` (59, 11224.350159 against 11224.357422), whose trades from
//! 2019 had no rate before; `loft_strategy_v4` (from 2021-01) is taken on the owner's word with its own numbers
//! (`strategies::ACCEPTED`).
//!
//! Left out: `trend_reversal_alerts_strategy_4h_3m`, 2613 trades and -56661.39 against 2610 and -46394.82, the same on a
//! replay outside the engine and under every reading of its rule tried (the set's notes, outside the repo, list them).
//! And `moving_average_band_strategy`, version 6 at 100%: 2477 and 13659.60 with the margin call against 2485 and
//! 10223.97 (1477 and 29866.36 without it). A replay outside the engine that also cancels a close issued with an entry the
//! margin refuses gets 2484 and 9994.86, so the rule is not yet whole for it. Its file, the call on, is in `63ab84ac92`.
//!
//! # Other markets and timeframes (`markets.rs`)
//!
//! A row's market and bar length are its reference's `symbol` and `timeframe` (`reference_b.csv`), not constants: `run` sets them
//! (`markets::set`) before it builds the rule, and they reach the shared desk through `Terms::symbol`, `Terms::bar_ms` and
//! `Terms::tick`. Every BTCUSDT 30-minute row runs exactly as before (the full run's row lines are the same). What differs by
//! symbol is the venue's own instrument facts, its kept row read through the datahub (`markets.rs`), never a table here: the
//! quantity step (`sizing.rs` floors to it: BTC 0.00001, ETH 0.0001, LTC 0.001, DOGE 1 on Binance spot) and the tick (slippage
//! and the stop and limit grid count in it). A row on another venue's series reads that venue's bars and row
//! (`BYBIT:WIFUSDT.P`, Bybit's linear perpetual), held in the lab's store by its name as every series is. The rules above
//! held on every other market and bar length a row was written for: the 4-hour and daily rows of BTCUSDT match with the same
//! window, no-bars-before-it and open-trade rules (`pts_golden_edge`, `range_strat_macd_rsi`, `bollinger_bands_enhanced_strategy`,
//! `triple_cci_strategy_mfi_confirmed_skyrexio`, `ma_10_20_crossover`, `supertrend_cloud_strategy`,
//! `intellect_city_world_cycle_ath_atl_timeframe_1d`, `london_breakout_classic`, `macd_ashi_bars_w_th_sl_tp`,
//! `ssl_exit_arrow_strategy`), as do one DOGEUSDT row on a stated size (`ema_cross_rsi_adx_strategy_v2`) and one ETHUSDT
//! 30-minute row (`trendmaster_pro_2_3_with_alerts`). The warm-up control starts at the market's first bar (`read_bars`). A row that wants a higher timeframe on a chart that is not the
//! 30-minute one reads `timeframe::Higher`, which takes the chart's bar length from the market, and its venue history is the
//! row's own market's (`timeframe::venue_bars`), as is each input a rule declares at another bar length of its own symbol
//! (`vike_tactics::Rule::inputs`, `"1d"`), which the store holds beside the charts. `Higher::looking_ahead` is the lookahead-on
//! reading: a higher bar on the first chart bar of its period, at its final values.
//!
//! Left out on these markets, each a strategy file measured on 2026-11-15 and not taken (the numbers are trades, then net,
//! against the reference's; the set's notes outside the repo hold each file and its readings):
//! - `ik_grid_script` (DOGEUSDT, USD): the rule closes the entry its id names (`close_entries_rule` ANY) and the desk closes
//!   first in, first out, which splits lots of different sizes into more rows: 133 and 46.60 against 68 and 49.64365. A replay
//!   outside the engine that closes by id gets 68, and a net of 49.37 to 49.70 by the quantity step it floors to, none exact.
//!
//! Taken though they miss, on the owner's word of 2026-11-15, each passing on its own exact numbers in `strategies::ACCEPTED`
//! (with its reason): `rci_strategy_...`, `fibonacci_trend_reversals`, `hulk_grid_algorithm_v2_the_quant_science`,
//! `high_low_channel_swing`, `pt_premarket_breakout_strategy` and `volume_difference_delta_cycle_oscillator`. Every margin-call
//! cut of the first two equals a twin of the reference's rules; they miss elsewhere (their rows say where).
//!
//! Taken though they miss, on the owner's word of 2026-11-16 ("ARE MATCHED"), each passing on its own exact numbers in
//! `strategies::ACCEPTED`: the five rows of `CAPITALCOM:EURUSD`, run on Dukascopy's EURUSD (`markets.rs`'s stand-in, its
//! 30-minute, 4-hour and daily bars built from the 15-minute ones on the FX day). Another feed's prices: the counts come at
//! most 16 trades off (one exact) and no net within the tolerance, on bars of the mid price and of the bid alike. Each row runs
//! on the side that came nearer its reference's count, the mid price but for `lanz_strategy_2_0_backtest` (bid: 41 and 175.58
//! against 38 and 140.75, mid 45 and 135.74), whose window holds the held series' hole of 2026-09-23 00:00 to 06:32 UTC.
//! `strategy_rs_open_session_breakout_trader` reads its sessions on New York time and its days as the FX days.
//!
//! The rows of a US share (`NASDAQ:AAPL`, `AMEX:SPY`) run on Dukascopy's CFD of it (`markets.rs`' stand-ins): another feed's
//! prices, mid quotes of the regular session, AAPL's read split-adjusted by the operator's split row (decision 0289). None of
//! the six matches (IMP-I7b, 2026-11-16, the AAPL rows measured again on adjusted bars on 2026-11-17; the numbers are in the
//! set's notes outside the repo). On the owner's word of 2026-11-17 `joint_conditions_strategy_suite_tradingconnector` (AAPL
//! 30 minutes) is taken, passing on its own exact numbers in `strategies::ACCEPTED`, and two are left out:
//! `gap_filling_strategy` (AAPL 30 minutes), 777 trades and -127.01 against 906 and -77.06, and `top_40_high_low_strategy`
//! (SPY 5 minutes, reading 39 share CFDs), 246 and -19432.00 against 247 and -28164.50.
//! `opening_range_breakout_orb_heikin_ashi_spy_5min`, a BTCUSDT row that reads SPY's CFD as an input of another series,
//! matches its reference's zero trades.
//!
//! An exit that names an entry another exit already took the whole quantity of fills nothing: `optimized_heikin_ashi_...`'s
//! target never fills, the stop does (the row matches only so).
//!
//! The rows run on `--threads N` threads (by default the cores, at most `DEFAULT_THREADS`), a row and all its control runs on
//! one, whose control switches and market are its own (`timeframe.rs`, `sizing.rs` and `markets.rs` keep them per thread). The
//! lines print in the rows' order, so the output is the same for every N; `--threads 1` runs the rows one after another. A
//! control whose switch the row's own run never read is not run (`runs_of`): it would give the row's numbers, which separate
//! nothing. `--quick` runs each row's own run alone and reports no control: for a check between full runs.
//!
//! Every bar is the venue's own, read through the datahub (`read_bars`, over set A's `bars.rs`): the chart of each row, its
//! market's daily, 4-hour and 30-minute bars for a higher timeframe, BTCUSDT's daily bars, and each input a wanted rule
//! declares of its own market at another bar length, the 1-minute bars of a row under the magnifier (its detail series)
//! among them, and each series of another market a rule declares (`VENUE:SYMBOL:INTERVAL`), whole, held under its venue (its
//! own gaps tolerated: desk's `other_gap`). Every run is a backtest profile's run
//! through `vike_backtest` (set A's `desk.rs`'s `//!`), over the charts read once (`desk::store_of`); its trades are counted in
//! the engine's closed-trade ledger, a USD account's on the lab's own booking (`desk::Terms::account_usd`). A USD account's
//! USDT-USD rates come through the datahub too, from the lane `RATES_LANE` (`read_rates`); no file but the reference is read.
//!
//! usage: cargo run -p vike-strategy-lab --example strategy_lab_b -- <dir with reference_b.csv> [--threads N] [--quick] [--dump DIR]
//! [name...]; a USD account's rates are the datahub's `binance USDTUSD 1d` bars in the lane `user-coinbase`.

#[path = "../strategy_lab/bars.rs"]
mod bars;
#[expect(
    dead_code,
    reason = "set A's list comparison, its drills and the verbs no set-B row uses yet"
)]
#[path = "../strategy_lab/desk.rs"]
mod desk;
mod markets;
mod sizing;
mod strategies;
mod timeframe;

use std::cell::Cell;
use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, OnceLock};
use std::thread;

use desk::Terms;
use markets::{Chart, Market};
use strategies::{Listed, PlanEntry};
use vike_data::store::series::parse_input_name;
use vike_data::{HistoryStore, SeriesId, TimestampRange};
use vike_indicators::strategy_kit::with_heikin_ashi_candles_source;
use vike_marketdata::Bar;
use vike_model::{Commission, TradeClosedRow};
use vike_tactics::higher;
use vike_trade_ledger::TradeClosedEntry;

/// The market and the bar length `Terms::PLAIN` starts from, set B's first chart (Binance's BTCUSDT 30-minute bars, its
/// `FIRST_CHART`); a run takes both from its row (`markets.rs`).
const SYMBOL: &str = "BTCUSDT";
const INTERVAL: &str = "30m";
const BAR_MS: i64 = 1_800_000;
/// `SYMBOL`'s market and the reference's name of its bar length, as a reference row names them.
const FIRST_CHART: (&str, &str) = ("BINANCE:BTCUSDT", "30");
/// The bar length the magnifier walks a bar through (`Terms::magnifier`).
const MINUTE: &str = "1m";
/// The cost control's wrong commission, in percent.
const WRONG_COMMISSION: f64 = 0.1;
/// Coinbase's USDT-USD daily candles, read only when a USD account runs: the operator's own `csv-bars` import into this lane of
/// the datahub's store (`RATES_IMPORT`). Their terms allow research use and forbid passing them on: never commit, mirror or
/// ship them. The lane's venue is the one whose USDT amounts they convert, as an import's venue must be on the roster, which has
/// no Coinbase.
const RATES_LANE: &str = "user-coinbase";
const RATES_VENUE: &str = "binance";
const RATES_SYMBOL: &str = "USDTUSD";
/// The import that fills `RATES_LANE`, run on the datahub's box from the project directory, the candles' CSV (`ts`, `open`,
/// `high`, `low`, `close`, `volume`) placed in `<project>/market_data/imports/csv-bars/USDT-USD/`.
const RATES_IMPORT: &str = "bin/vike-cli data history import csv-bars USDT-USD --venue binance --symbol USDTUSD \
                            --interval 1d --lane user-coinbase --yes";
/// The threads a run takes without `--threads`, fewer on a box with fewer cores: the lanes' boxes are shared, and 8 is their
/// `LANE_NEXTEST_THREADS`.
const DEFAULT_THREADS: usize = 8;
/// The history every series is read over, pinned so that it stays the one every number here was measured on as the datahub's
/// store grows. It starts with 2024-12-01 00:00 UTC, except a 4-hour or daily series and a stand-in's (`markets::is_stand_in`, the
/// operator's import of the reference windows), which start with the series' first bar on the venue. It ends with 2026-10-31 UTC.
const HISTORY_FROM_MS: i64 = 1_733_011_200_000;
const HISTORY_TO_MS: i64 = 1_793_491_199_999;

/// One round trip, as `desk` books it.
#[derive(Clone, Debug)]
#[expect(dead_code, reason = "read by desk's trade-list comparison, which set B has no lists for")]
struct Trade {
    dir: i32,
    entry_ms: i64,
    entry_price: f64,
    exit_ms: i64,
    exit_price: f64,
}

#[expect(dead_code, reason = "as `Trade`")]
impl Trade {
    fn same_as(&self, o: &Trade) -> bool {
        self.dir == o.dir
            && self.entry_ms == o.entry_ms
            && self.exit_ms == o.exit_ms
            && (self.entry_price - o.entry_price).abs() < 1e-9
            && (self.exit_price - o.exit_price).abs() < 1e-9
    }
}

struct Reference {
    /// The market and bar length of the row's chart.
    chart: Chart,
    from_ms: i64,
    to_ms: i64,
    trades: usize,
    net: f64,
}

impl Reference {
    /// How far a net may lie from the reference's and still match it.
    fn tolerance(&self) -> f64 {
        0.01_f64.max(1.2e-7 * self.net.abs())
    }

    fn matches(&self, trades: usize, net: f64) -> bool {
        trades == self.trades && (net - self.net).abs() <= self.tolerance()
    }

    /// Two readings of a row this reference can tell apart: no reference matches both, so other counts, or nets further
    /// apart than twice the tolerance. Two nets closer than that could both match (an open trade's entry commission of 0.01
    /// is the standing case), and then the row says nothing about which reading is right.
    fn separates(&self, a: (usize, f64), b: (usize, f64)) -> bool {
        a.0 != b.0 || (a.1 - b.1).abs() > 2.0 * self.tolerance()
    }
}

/// `market`'s `interval` bars over `HISTORY_FROM_MS ..= HISTORY_TO_MS`, through the datahub; a market built from a finer
/// series (`markets::Market::built_from`) builds them from all of that series to `HISTORY_TO_MS` (`timeframe::built`), one
/// built from the bid side of the venue's ticks from those from `HISTORY_FROM_MS` (`bid_quarter_hours`).
fn read_bars(market: Market, interval: &str) -> Vec<Bar> {
    let Market { venue, series, .. } = market;
    let bar_ms = || markets::interval_ms(interval);
    match market.built_from {
        Some(markets::BID) => timeframe::built(bid_quarter_hours(market), bar_ms()),
        Some(finer) => {
            timeframe::built(&bars::read(venue, series, finer, None, HISTORY_TO_MS), bar_ms())
        }
        None => {
            let whole = matches!(interval, "4h" | "1d") || markets::is_stand_in(market);
            let from = (!whole).then_some(HISTORY_FROM_MS);
            bars::read(venue, series, interval, from, HISTORY_TO_MS)
        }
    }
}

/// A connection of its own to the datahub `bars.rs` reads from, on the same address and with this box's
/// device key (decision 0288 item 8: no person or tool reaches a hub with a shared key).
fn datahub_client() -> vike_datahub_client::DatahubClient {
    use vike_datahub_client::route;
    let env: HashMap<String, String> = std::env::vars().collect();
    let route = route::history_route(route::datahub_address_for_bin(&env).as_deref());
    let store = vike_secrets::open_credential_store(&env).unwrap_or_else(|e| panic!("{e}"));
    let device = match store.location() {
        Some(location) => vike_secrets::access::device_key_load(location.settings_dir()),
        None => Ok(None),
    };
    let keys = route::routed_hub_keys(device, route.hub());
    let scope = vike_hub_protocol::auth::Scope::Read;
    vike_datahub_client::DatahubClient::connect_with_keys(route.hub(), keys.as_ref(), scope)
        .unwrap_or_else(|e| panic!("the datahub at {}: {e}", route.hub()))
}

/// `market`'s quote ticks over `HISTORY_FROM_MS ..= HISTORY_TO_MS` folded into 15-minute bars of the BID (each opening at its
/// first tick's bid, closing at its last one's), once per process. Read through the datahub one UTC day a request, unpaged (a
/// day is one frame; the paged `vike_data::HistoryStore::scan_quotes` re-reads a long window once per page), a quarter of
/// days on each of several connections at once. shortcut: from `HISTORY_FROM_MS` only, which a 30-minute row needs; a 4-hour
/// or daily row on the bid needs its read from the series' first tick.
fn bid_quarter_hours(market: Market) -> &'static [Bar] {
    static BARS: OnceLock<(&str, Vec<Bar>)> = OnceLock::new();
    let (series, bars) = BARS.get_or_init(|| {
        const DAY_MS: i64 = 86_400_000;
        const SPAN_DAYS: i64 = 91;
        let days = (HISTORY_TO_MS - HISTORY_FROM_MS) / DAY_MS + 1;
        let spans: Vec<(i64, i64)> =
            (0..days).step_by(SPAN_DAYS as usize).map(|d| (d, (d + SPAN_DAYS).min(days))).collect();
        let read = |(first, last): (i64, i64)| -> Vec<Bar> {
            let mut client = datahub_client();
            let mut out = Vec::new();
            for day in first..last {
                let from = HISTORY_FROM_MS + day * DAY_MS;
                let ticks = client
                    .scan_quotes(
                        market.venue,
                        market.series,
                        TimestampRange::of(from, from + DAY_MS - 1),
                        None,
                    )
                    .unwrap_or_else(|e| panic!("{} {} quotes: {e}", market.venue, market.series));
                let one_price: Vec<Bar> = ticks
                    .iter()
                    .map(|t| Bar::ohlcv(t.timestamp, t.bid, t.bid, t.bid, t.bid, 1.0))
                    .collect();
                out.extend(vike_marketdata::consolidator::resample(&one_price, 900_000));
            }
            out
        };
        let parts: Vec<Vec<Bar>> = thread::scope(|s| {
            let handles: Vec<_> = spans.iter().map(|&span| s.spawn(move || read(span))).collect();
            handles.into_iter().map(|h| h.join().expect("a reader thread")).collect()
        });
        let out: Vec<Bar> = parts.into_iter().flatten().collect();
        assert!(!out.is_empty(), "the datahub holds no {} {} quote", market.venue, market.series);
        assert!(out.windows(2).all(|w| w[0].timestamp < w[1].timestamp), "the bid bars in order");
        (market.series, out)
    });
    assert_eq!(*series, market.series, "one series' bid bars per process");
    bars
}

/// The reference rows on a market this harness has bars of, by name.
fn read_reference(path: &str) -> HashMap<String, Reference> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    let mut lines = text.lines();
    let head: Vec<&str> = lines.next().expect("a header").split(',').collect();
    let col =
        |name: &str| head.iter().position(|h| *h == name).unwrap_or_else(|| panic!("no {name}"));
    let (slug, symbol, timeframe) = (col("slug"), col("symbol"), col("timeframe"));
    let (from, to, trades, net) = (col("from_ms"), col("to_ms"), col("trades"), col("net_profit"));
    lines
        .filter(|l| !l.is_empty())
        .map(|l| l.split(',').collect::<Vec<_>>())
        .filter_map(|c| {
            Some((markets::on_row(c[slug], markets::chart_of(c[symbol], c[timeframe])?), c))
        })
        .map(|(chart, c)| {
            let reference = Reference {
                chart,
                from_ms: c[from].parse().unwrap_or_else(|e| panic!("{c:?}: {e}")),
                to_ms: c[to].parse().unwrap_or_else(|e| panic!("{c:?}: {e}")),
                trades: c[trades].parse().unwrap_or_else(|e| panic!("{c:?}: {e}")),
                // A row with no trades states no net.
                net: if c[net].is_empty() {
                    0.0
                } else {
                    c[net].parse().unwrap_or_else(|e| panic!("{c:?}: {e}"))
                },
            };
            (c[slug].to_string(), reference)
        })
        .collect()
}

/// `RATES_LANE`'s daily bars as the conversion pair of `desk::USD_RATES`, through the datahub up to `HISTORY_TO_MS`, its
/// whole history (the import held every bar to the UTC day's grid): read as the backtest's own conversion reads a pair, its
/// direction off its name (`vike_model::AccountPerQuoteLeg::of_pair_name`) and one-to-one wherever it has no rate, as the
/// backtest's rule says (`vike_backtest::run::data_plan::converts_one_to_one_where_missing`, decision 0243 amended 2026-11-16). A
/// lane it holds no bar of is refused, naming the import that fills it.
fn read_rates() -> vike_simulator::ConversionSeries {
    let series = format!("{RATES_VENUE} {RATES_SYMBOL} 1d in the lane {RATES_LANE}");
    let fill =
        format!("a USD account converts at those rates; fill the lane with `{RATES_IMPORT}`");
    let range = TimestampRange { start: None, end: Some(HISTORY_TO_MS) };
    let mut bars = bars::datahub()
        .load_bars_sourced_capped(RATES_VENUE, RATES_LANE, RATES_SYMBOL, "1d", range, None)
        .unwrap_or_else(|e| panic!("{series} through the datahub: {e}. {fill}"));
    assert!(!bars.is_empty(), "the datahub holds no {series}: {fill}");
    bars.sort_by_key(|b| b.timestamp);
    let (quote, ledger) = ("USDT", "USD");
    vike_simulator::ConversionSeries {
        quote_currency: quote.into(),
        pair: format!("{RATES_LANE}:{RATES_VENUE}:{RATES_SYMBOL}:1d"),
        leg: vike_model::AccountPerQuoteLeg::of_pair_name(RATES_SYMBOL, quote, ledger)
            .expect("USDTUSD is a USDT priced in dollars"),
        interval_ms: 86_400_000,
        bars: bars.into(),
        symbols: Vec::new(),
        par_where_missing: vike_backtest::run::data_plan::converts_one_to_one_where_missing(
            quote, ledger,
        ),
    }
}

/// A control's switch, kept per thread in a `thread_local!`, which notes whether a run read it: a run that never read it takes
/// the same path with it on, so its control would give the rule's own numbers and separate nothing (`runs_of` skips it).
pub struct Switch {
    on: Cell<bool>,
    read: Cell<bool>,
}

impl Switch {
    pub const fn off() -> Self {
        Switch { on: Cell::new(false), read: Cell::new(false) }
    }

    /// Whether the switch is on, noted as read.
    pub fn get(&self) -> bool {
        self.read.set(true);
        self.on.get()
    }

    /// Whether the thread read the switch since the last call.
    fn was_read(&self) -> bool {
        self.read.replace(false)
    }
}

/// The runs made, for the line `main` prints to stderr; no run reads it.
static RUNS: AtomicUsize = AtomicUsize::new(0);

/// One run: the trades closed by a fill, as the closed-trade ledger counted them (`desk::Booked`), and the ones still open on
/// the last bar, booked at its close.
#[derive(Default)]
struct Outcome {
    closed: Vec<TradeClosedEntry>,
    open: Vec<TradeClosedRow>,
    /// The entry commission the open trades paid, each at its entry bar's `Terms::rate`.
    open_entry_fee: f64,
}

impl Outcome {
    fn net(&self) -> f64 {
        self.closed.iter().map(|r| r.net).sum()
    }

    /// The closed trades and their net, what a reference is matched on.
    fn reading(&self) -> (usize, f64) {
        (self.closed.len(), self.net())
    }
}

/// Whether the row's account is kept in USD.
fn usd(row: &Listed) -> bool {
    (row.build)().terms().account_usd
}

fn run(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    row: &Listed,
    reference: &Reference,
    warm_from_ms: i64,
    reading: impl Fn(Terms) -> Terms,
) -> Outcome {
    RUNS.fetch_add(1, Ordering::Relaxed);
    // The row's market first: a rule's `terms` and every size it works out read it.
    let Chart { market, interval, bar_ms } = reference.chart;
    markets::set(reference.chart);
    let e = PlanEntry {
        name: row.name,
        warm_from_ms,
        trade_from_ms: Some(reference.from_ms),
        end_ms: reference.to_ms,
        build: row.build,
    };
    let plan = (e.build)();
    // The reference fills in the order the price meets the orders, and binds each exit to its entry (this file's `//!`).
    let terms = reading(Terms {
        price_order: true,
        bound_exits: true,
        symbol: market.symbol,
        interval,
        bar_ms,
        tick: market.tick,
        quantity_step: Some(sizing::quantity_step()),
        market_closes: market.closes,
        ..plan.terms()
    });
    let start = e.trade_from_ms.expect("stated above");
    // A lower series the reference read from a later bar is served from that bar on (rule 7).
    let later;
    let store = if row.inputs_from.is_empty() {
        store
    } else {
        later = from_later_bars(store, row, &plan.inputs(), &terms);
        &later
    };
    // Under the magnifier the desk walks each bar through its 1-minute bars, which the store holds (`main`).
    let minutes: Rc<[Bar]> = if terms.magnifier {
        let range =
            TimestampRange { start: Some(e.warm_from_ms), end: Some(e.end_ms + bar_ms - 1) };
        store
            .load_bars(desk::VENUE, market.symbol, MINUTE, range)
            .unwrap_or_else(|e| panic!("{} {MINUTE} from the lab's store: {e}", market.symbol))
            .into()
    } else {
        Rc::from([])
    };
    let booked = desk::run_split(store, &minutes, plan, terms, e.warm_from_ms, start, e.end_ms);
    let desk::Booked { closed, open, .. } = booked;
    let open_entry_fee = open
        .iter()
        .map(|r| {
            desk::charge(terms.commission, r.qty, r.entry_price) * terms.rate(r.entry_timestamp)
        })
        .sum();
    Outcome { closed, open, open_entry_fee }
}

/// The store a row runs on whose reference read a lower series from a later bar than the lab holds (`Listed::inputs_from`,
/// rule 7): its chart and every series of its own symbol its rule reads (and the magnifier's minutes), each listed one from
/// its listed bar on, the rest whole.
fn from_later_bars(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    row: &Listed,
    inputs: &[String],
    terms: &Terms,
) -> Arc<dyn HistoryStore + Send + Sync> {
    let mut names = vec![terms.interval];
    names.extend(inputs.iter().map(String::as_str).filter(|name| !name.contains(':')));
    if terms.magnifier {
        names.push(MINUTE);
    }
    names.sort_unstable();
    names.dedup();
    for (input, _) in row.inputs_from {
        assert!(
            names.contains(input),
            "{}: `inputs_from` names {input}, which its rule does not read",
            row.name
        );
    }
    let whole = TimestampRange { start: None, end: None };
    let series: Vec<(&str, Vec<Bar>)> = names
        .into_iter()
        .map(|name| {
            let mut bars = store
                .load_bars(desk::VENUE, terms.symbol, name, whole)
                .unwrap_or_else(|e| panic!("{} {name} from the lab's store: {e}", terms.symbol));
            if let Some(&(_, from)) = row.inputs_from.iter().find(|(input, _)| *input == name) {
                bars.retain(|b| b.timestamp >= from);
            }
            (name, bars)
        })
        .collect();
    desk::store_of(series.iter().map(|(name, bars)| (terms.symbol, *name, bars.as_slice())))
}

/// One row's runs, all on one thread: the rule taken, then each reading a control tries, as `Outcome::reading`. `None` is a
/// control not run, because it would give the rule's own numbers or under `--quick`: it separates nothing.
#[derive(Default)]
struct Runs {
    taken: Outcome,
    early: Option<(usize, f64)>,
    raw: Option<(usize, f64)>,
    /// Under `timeframe::IN_PROGRESS`, `WINDOW_ONLY`, `INTRADAY_HISTORY` and `UNROUNDED_CANDLES`, in that order.
    flagged: [Option<(usize, f64)>; 4],
    /// The cost control's row only: the wrong commission.
    wrong: Option<(usize, f64)>,
    /// A USD account's row only: booked in USDT.
    quoted: Option<(usize, f64)>,
}

/// Every run of one row, on the calling thread, each control's switch on around its own run only. A control whose switch the
/// rule's run never read is not run, nor the warm-up when the window starts at the first bar on file (the same run): both would
/// give the rule's numbers. The fee, the USD and the unrounded-size controls change a parameter, not a switch, and always run.
fn runs_of(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    bars: &[Bar],
    row: &Listed,
    reference: &Reference,
    quick: bool,
) -> Runs {
    let same = |terms: Terms| terms;
    let switches = [
        &timeframe::IN_PROGRESS,
        &timeframe::WINDOW_ONLY,
        &timeframe::INTRADAY_HISTORY,
        &timeframe::UNROUNDED_CANDLES,
    ];
    for switch in switches {
        switch.with(Switch::was_read);
    }
    desk::DUMP_ROW.with(|r| *r.borrow_mut() = Some(row.name.to_string()));
    let taken = run(store, row, reference, reference.from_ms, same);
    // What the rule's run read, taken before any control runs, which reads switches of its own.
    let read = switches.map(|switch| (switch, switch.with(Switch::was_read)));
    if quick {
        return Runs { taken, ..Runs::default() };
    }
    let early = (bars[0].timestamp != reference.from_ms)
        .then(|| run(store, row, reference, bars[0].timestamp, same).reading());
    // A rule sizes through the order language, which no switch of this harness reaches: every row runs without the quantity
    // step, and a row that never floors a size gives its own numbers, which separate nothing.
    let unrounded = |terms: Terms| Terms { quantity_step: None, ..terms };
    let raw = Some(run(store, row, reference, reference.from_ms, unrounded).reading());
    // A rule the gallery took reads its inputs, which no switch reaches: it runs again on this harness's frames
    // (`timeframe::LabFrames`), every switch off, which must give its own numbers to the bit, and what that run read is what
    // the controls flip, on those frames (`timeframe.rs`'s `//!`).
    let ahead = timeframe::AHEAD_ROWS.contains(&row.name);
    let frames = Rc::new(timeframe::LabFrames { ahead, made: Cell::new(false) });
    // The frames and the candles are two sources (the candles' is the indicators', `vike_indicators::strategy_kit`), installed
    // together.
    let on_both = |f: &dyn Fn() -> Outcome| {
        higher::with_higher_timeframe_source(frames.clone(), || {
            with_heikin_ashi_candles_source(frames.clone(), f)
        })
    };
    higher::with_higher_timeframe_source(frames.clone(), || {
        with_heikin_ashi_candles_source(frames.clone(), || drop((row.build)()))
    });
    let on_frames = |f: &dyn Fn() -> Outcome| if frames.made.get() { on_both(f) } else { f() };
    let read = if frames.made.get() {
        for switch in switches {
            switch.with(Switch::was_read);
        }
        let own = on_frames(&|| run(store, row, reference, reference.from_ms, same)).reading();
        let (count, net) = taken.reading();
        assert!(
            own.0 == count && own.1.to_bits() == net.to_bits(),
            "{}: this harness's frames give {own:?}, its inputs ({count}, {net})",
            row.name
        );
        switches.map(|switch| (switch, switch.with(Switch::was_read)))
    } else {
        read
    };
    let flagged = read.map(|(switch, read)| {
        read.then(|| {
            switch.with(|s| s.on.set(true));
            let other =
                on_frames(&|| run(store, row, reference, reference.from_ms, same)).reading();
            switch.with(|s| s.on.set(false));
            other
        })
    });
    let wrong = (row.name == strategies::COST_CONTROL).then(|| {
        let fee =
            |terms: Terms| Terms { commission: Commission::percent(WRONG_COMMISSION), ..terms };
        run(store, row, reference, reference.from_ms, fee).reading()
    });
    let quoted = usd(row).then(|| {
        let quoted = |terms: Terms| Terms { account_usd: false, ..terms };
        run(store, row, reference, reference.from_ms, quoted).reading()
    });
    Runs { taken, early, raw, flagged, wrong, quoted }
}

/// A reading the reference refutes: `separated` rows told it apart from the rule taken, and every one of them must miss.
#[derive(Default)]
struct Control {
    separated: usize,
    missed: usize,
}

impl Control {
    /// `differs`: the reading gives numbers the reference can tell from the rule taken on this row (`Reference::separates`);
    /// `matched`: they match the reference.
    fn note(&mut self, differs: bool, matched: bool) {
        if differs {
            self.separated += 1;
            self.missed += usize::from(!matched);
        }
    }

    /// `false` when a row that separates the reading matched it, or when a full run had no row to separate it.
    fn report(&self, name: &str, full_run: bool) -> bool {
        let ok = self.missed == self.separated && (self.separated > 0 || !full_run);
        let word = if !ok {
            "FAIL"
        } else if self.separated == 0 {
            "INFO"
        } else {
            "PASS"
        };
        println!(
            "{word} control: {name}: {} of {} rows that separate it miss",
            self.missed, self.separated
        );
        ok
    }
}

fn main() {
    markets::self_check();
    sizing::self_check();
    timeframe::self_check();
    strategies::self_check();
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: strategy_lab_b <dir> [--threads N] [--quick] [--dump DIR] [name...]");
    let mut only: Vec<String> = args.collect();
    if let Some(i) = only.iter().position(|a| a == "--dump") {
        let to = only.drain(i..(i + 2).min(only.len())).nth(1).expect("--dump takes a directory");
        fs::create_dir_all(&to).unwrap_or_else(|e| panic!("{to}: {e}"));
        desk::DUMP_DIR.set(to).expect("one dump directory");
    }
    let quick = only.iter().position(|a| a == "--quick").map(|i| only.remove(i)).is_some();
    let threads = match only.iter().position(|a| a == "--threads") {
        Some(i) => only
            .drain(i..(i + 2).min(only.len()))
            .nth(1)
            .and_then(|n| n.parse::<NonZeroUsize>().ok())
            .expect("--threads takes a positive whole number")
            .get(),
        None => thread::available_parallelism().map_or(1, NonZeroUsize::get).min(DEFAULT_THREADS),
    };
    let references = read_reference(&format!("{dir}/reference_b.csv"));
    let wanted = |row: &Listed| only.is_empty() || only.iter().any(|s| s == row.name);
    let reference_of = |row: &Listed| {
        references.get(row.name).unwrap_or_else(|| panic!("no reference for {}", row.name))
    };
    // The bars of every chart a wanted row runs on, each series read once. BTCUSDT 30 minutes comes first and prints the line
    // this harness always printed.
    let first = markets::chart_of(FIRST_CHART.0, FIRST_CHART.1)
        .expect("the kept lists carry set B's first market");
    let mut charts = vec![first];
    for row in strategies::PLANS.iter().filter(|&row| wanted(row)) {
        let chart = reference_of(row).chart;
        if !charts.contains(&chart) {
            charts.push(chart);
        }
    }
    // The lab's store holds every series by its name (`desk::store_of`).
    let series = |chart: Chart| (chart.market.symbol, chart.interval);
    let mut bars_of: BTreeMap<(&str, &str), Vec<Bar>> = BTreeMap::new();
    for chart in charts {
        let bars = read_bars(chart.market, chart.interval);
        if chart == first {
            println!("bars read: {}", bars.len());
        } else {
            println!("bars read: {} ({} {})", bars.len(), chart.market.symbol, chart.interval);
        }
        bars_of.insert(series(chart), bars);
    }
    // A gallery rule reads a day of BTCUSDT as its input `1d` (`vike_tactics::higher`): the venue's daily bars from the
    // symbol's first, beside the charts, the series `timeframe::days` reads for a rule kept here.
    let daily = (first.market.symbol, "1d");
    let days =
        (!bars_of.contains_key(&daily)).then(|| timeframe::venue_bars(first.market, daily.1));
    // A rule's run reads each input it declares of its own symbol at another bar length (`vike_tactics::Rule::inputs`, a bare
    // interval: `"1d"`), and a row under the magnifier (`Terms::magnifier`, `data.detail_interval`) its market's 1-minute
    // bars, from the store: that series of the row's market, read once, beside the charts.
    let mut inputs: BTreeMap<(&str, &str), &[Bar]> = BTreeMap::new();
    // Another series a rule declares (`VENUE:SYMBOL:INTERVAL`, decision 0271): that venue's bars, whole, held under that venue as
    // the run's profile reads them.
    let mut others: BTreeMap<String, (SeriesId, Vec<Bar>)> = BTreeMap::new();
    for row in strategies::PLANS.iter().filter(|&row| wanted(row)) {
        let market = reference_of(row).chart.market;
        markets::set(reference_of(row).chart);
        let plan = (row.build)();
        let detail = plan.terms().magnifier.then(|| "1m".to_string());
        for name in plan.inputs().into_iter().chain(detail) {
            if name.contains(':') {
                if let Entry::Vacant(slot) = others.entry(name) {
                    let (id, _) = parse_input_name(slot.key())
                        .unwrap_or_else(|e| panic!("{}: {e}", row.name));
                    let interval = id.interval.as_deref().expect("a bar series");
                    let bars = bars::read(&id.venue, &id.symbol, interval, None, HISTORY_TO_MS);
                    println!("bars read: {} ({} {} {interval})", bars.len(), id.venue, id.symbol);
                    slot.insert((id, bars));
                }
                continue;
            }
            let Some(interval) = markets::interval_named(&name) else { continue };
            let key = (market.symbol, interval);
            if bars_of.contains_key(&key) || key == daily || inputs.contains_key(&key) {
                continue;
            }
            let bars = timeframe::venue_bars(market, interval);
            println!("bars read: {} ({} {})", bars.len(), market.symbol, interval);
            inputs.insert(key, bars);
        }
    }
    let store = desk::store_of(
        bars_of
            .iter()
            .map(|(&(symbol, interval), bars)| (symbol, interval, bars.as_slice()))
            .chain(days.map(|bars| (daily.0, daily.1, bars)))
            .chain(inputs.iter().map(|(&(symbol, interval), &bars)| (symbol, interval, bars))),
    );
    for (id, bars) in others.values() {
        let interval = id.interval.as_deref().expect("a bar series");
        store.append_bars(&id.venue, &id.symbol, interval, bars, None).unwrap_or_else(|e| {
            panic!("{} {} {interval} into the lab's store: {e}", id.venue, id.symbol)
        });
    }
    // Each rule is built on its own row's market (`markets::set`): a size or a margin it states reads the market's step.
    let reads_usd = |row: &Listed| {
        markets::set(reference_of(row).chart);
        usd(row)
    };
    if strategies::PLANS.iter().any(|row| wanted(row) && reads_usd(row)) {
        desk::USD_RATES.set(read_rates()).expect("the rates are read once");
    }

    let mut all_ok = true;
    let mut ran = 0;
    let mut warm = Control::default();
    let mut unrounded = Control::default();
    let mut counted = Control::default();
    let mut marked = Control::default();
    let mut fee_off = Control::default();
    let mut cost = Control::default();
    let mut unconverted = Control::default();
    let mut in_progress = Control::default();
    let mut window_only = Control::default();
    let mut intraday_history = Control::default();
    let mut unrounded_candles = Control::default();
    let jobs: Vec<(&Listed, &Reference, &[Bar])> = strategies::PLANS
        .iter()
        .filter(|&row| wanted(row))
        .map(|row| {
            let reference = reference_of(row);
            (row, reference, bars_of[&series(reference.chart)].as_slice())
        })
        .collect();
    // Each worker takes the next row not yet taken; a row's lines print once every row before it has.
    let next = AtomicUsize::new(0);
    let (done, finished) = mpsc::channel();
    thread::scope(|s| {
        for _ in 0..threads {
            let (done, jobs, next, store) = (done.clone(), &jobs, &next, &store);
            // The main thread's 8 MiB of stack, which every row ran on before.
            thread::Builder::new()
                .stack_size(8 << 20)
                .spawn_scoped(s, move || {
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(&(row, reference, bars)) = jobs.get(i) else { break };
                        let runs = runs_of(store, bars, row, reference, quick);
                        done.send((i, runs)).expect("main receives");
                    }
                })
                .expect("a worker thread");
        }
        drop(done);
        let mut waiting = BTreeMap::new();
        for (i, runs) in finished {
            waiting.insert(i, runs);
            while let Some(runs) = waiting.remove(&ran) {
                let (row, reference, _) = jobs[ran];
                ran += 1;
                let Runs { taken: o, early, raw, flagged, wrong, quoted } = runs;
                let (trades, net) = o.reading();
                // A row the owner accepted passes on exactly its recorded numbers, never on the tolerance, and fails
                // once it matches its reference (it then leaves `ACCEPTED`).
                let accepted = strategies::ACCEPTED.iter().find(|a| a.name == row.name);
                let ok = match accepted {
                    Some(a) => a.holds(trades, net) && !reference.matches(trades, net),
                    None => reference.matches(trades, net),
                };
                all_ok &= ok;
                println!(
                    "{} {:<48} trades vike={trades} ref={} net vike={net:.6} ref={:.6} diff={:.6} open={}",
                    if ok { "PASS" } else { "FAIL" },
                    row.name,
                    reference.trades,
                    reference.net,
                    net - reference.net,
                    o.open.len(),
                );
                if let Some(a) = accepted {
                    println!(
                        "     ACCEPTED ({}): recorded {} / {:?}, this run {trades} / {net:?}",
                        a.why, a.trades, a.net
                    );
                    // A reading that misses a reference the row itself misses refutes nothing: no control.
                    continue;
                }

                // Each reading the reference refutes, run or worked out on this row.
                let mine = (trades, net);
                let note = |control: &mut Control, reading: Option<(usize, f64)>| {
                    if let Some(reading) = reading {
                        control.note(
                            reference.separates(reading, mine),
                            reference.matches(reading.0, reading.1),
                        );
                    }
                };
                note(&mut warm, early);
                note(&mut unrounded, raw);
                let flag_controls = [
                    &mut in_progress,
                    &mut window_only,
                    &mut intraday_history,
                    &mut unrounded_candles,
                ];
                for (control, other) in flag_controls.into_iter().zip(flagged) {
                    note(control, other);
                }
                let open_net: f64 = o.open.iter().map(|r| r.net).sum();
                note(&mut counted, Some((trades + o.open.len(), net)));
                note(&mut marked, Some((trades, net + open_net)));
                note(&mut fee_off, Some((trades, net - o.open_entry_fee)));
                if let Some((wrong_trades, wrong_net)) = wrong {
                    let net_only = wrong_trades == trades && !reference.matches(trades, wrong_net);
                    cost.note(true, !net_only);
                }
                note(&mut unconverted, quoted);
            }
        }
    });
    assert!(
        ran > 0 && (only.is_empty() || ran == only.len()),
        "ran {ran} of the named strategies {only:?}"
    );
    eprintln!("{} runs of {ran} rows on {threads} threads", RUNS.load(Ordering::Relaxed));

    let full = only.is_empty();
    if quick {
        println!("controls skipped (--quick): a full run checks them");
    } else {
        all_ok &= warm.report("warm-up from the first bar on file", full);
        all_ok &= unrounded.report("unrounded sizes", full);
        all_ok &= counted.report("open trade counted", full);
        all_ok &= marked.report("open trade in the net, marked at the last close", full);
        all_ok &= fee_off.report("open trade's entry commission taken off the net", full);
        all_ok &= cost
            .report(&format!("cost: {WRONG_COMMISSION}% keeps the count and loses the net"), full);
        all_ok &= unconverted.report("USD account booked in USDT, unconverted", full);
        all_ok &= in_progress.report("higher bar in progress, at its final values", full);
        all_ok &= window_only.report("higher bars from the window only", full);
        all_ok &= intraday_history
            .report("intraday higher bars and Heikin-Ashi candles from the venue's history", full);
        all_ok &= unrounded_candles.report("Heikin-Ashi candles off the tick", full);
    }
    println!("{}", if all_ok { "RESULT: ALL MATCH" } else { "RESULT: DIFFERENCES FOUND" });
    std::process::exit(i32::from(!all_ok));
}
