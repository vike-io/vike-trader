//! A strategy that places real orders on `OrderBrokerSimulator`, checked trade by trade including quantity and net result. A rule is
//! written in the platform's order language (`vike_tactics`, decision 0269): its `Plan` has the language's own hooks,
//! `on_bar(strategy, bar)` on the tactics object. The ENGINE decides every trigger and every price, and the order in which one
//! bar meets them (the simulator's in-bar fill rules, `vike_simulator::InBarRules`); this module books the rows a list is compared on
//! and touches none of the engine's state. A rule that would need it to work out which order fires, or at what price, is out
//! of scope: report what the engine lacks.
//!
//! # A run is a backtest profile's (decision 0269, wave 4)
//!
//! Every run, each row's and each drill's, goes through `vike_backtest`: `run_split` writes the row's `Terms` as a backtest
//! profile's keys (`profile_of`; `Terms`' doc maps each), loads it through the profile's one door, hands the rule over as the
//! profile's strategy (`ProfileStrategy::builder`, `run_profile`) and runs it over the lab's bars (`store_of`: read once from the
//! datahub, served to each run over its own window). Its rows are counted in the engine's closed-trade ledger
//! (`vike_trade_ledger`, `engine.closed_trades`, with `engine.whole_entry_fee`, and the lots still open on the last
//! bar), checked equal to the lab's own booking on every row (`differs`). The one exception is the owner's: a USD account
//! (`Terms::account_usd`, 49 rows of set B) is counted on the lab's own booking, which converts each amount at its bar's rate.
//!
//! # The gallery's rules (decision 0269, wave 5)
//!
//! A row whose rule the strategy gallery took runs its strategy file (`strategies/<category>/<name>.rs`, compiled by
//! `vike_strategy_lab::gallery`): `gallery_on` builds it by its name and runs it on the row's `Terms`, its header stating the commission and the pyramiding, and a signal
//! row's rule is `from_gallery`'s. The lab keeps the comparison, the terms, its ledger and the dump. A rule sizes through the
//! order language (`vike_tactics::TacticsSizing`): the run's slippage and quantity step reach it as `vike_tactics::RuleRun` options
//! (`Runner::new`), the commission as its header, and a USD account's money through `UsdBroker`, this lab's view of the
//! engine for that exception.
//!
//! What a rule the lab keeps reads of this lab rather than of the tactics object is `Lab`'s, taken with `Lab::current`. Each is a
//! read the order language does not give a rule, which is why its rule stays here:
//! - `done`, `banked`: the closed rows so far, which the engine folds for its report alone;
//! - `minutes`: the 1-minute bars inside this bar, which `data.detail_interval` feeds to the engine's fills alone;
//! - `worth`, `rate`: a USD account's money (the exception above). Any other account's worth is `vike_tactics::Tactics::equity`.
//!
//! # Writing a strategy
//!
//! A rule the strategy gallery takes is a strategy file (`strategies/trend/analyzer_anvil_percent_costs.rs` is
//! the shape) and one `PLANS` row here, `plan_row(name, last_bar, || gallery_on(name, terms))`; one it does not take is a file in
//! `strategies/`, one `mod` line and one `PLANS` row, `plan_row(name, last_bar, new)`. Set `PlanEntry::trade_from_ms` when the list's first trade is a resting entry, which may
//! have been placed long before it filled. The file implements `Plan`: `terms` states `Terms`, and an override in `inputs.json`
//! wins over the rule's header; `on_bar` runs on every bar, and the order verbs do nothing outside the trading window.
//!
//! - The verbs are `vike_tactics::Tactics`': `entry` (market, `.stop`, `.limit`, both a stop-limit), `close`, `close_all`
//!   (`.size`, `.here` for this bar's close), `exit` (`.from`, `.size`, `.stop`, `.take`, `.trail` a `vike_tactics::TacticsTrail`,
//!   `.group`), `order` (a free order), `cancel`, `cancel_all`, and the reads `position`, `avg_cost`, `lots`, `bindings`,
//!   `snap_stop`, `snap_limit`. The crate's page has their rules (`crates/vike-tactics/CLAUDE.md`).
//! - The account's worth is `vike_tactics::Tactics::equity`, a size from money `vike_tactics::Tactics::sizing`'s
//!   (`size_for_share`, `size_for_cash`, `slipped`). This lab's reads are `Lab`'s (above): `worth` and `rate` (a USD account),
//!   `banked`, `done`, `minutes`. Time: `vike_tactics::calendar::clock`.
//! - The rule again after each of its fills: implement `Plan::after_fill`. A day's fills: `Terms::fills_a_day`.
//! - Lower-timeframe values: set `PlanEntry::minutes` and read `Lab::minutes`, the 1-minute bars inside this bar. Set
//!   `Terms::magnifier` as well to walk those bars. They are read from `FROM_2025_03_31` on, and only when a row that wants them runs.
//! - A fill refused for want of cash: `Terms::cash_gate`, for a rule of market orders only.
//! - Set B's reference (`strategy_lab_b/main.rs`), each a switch set A leaves off: `Terms::price_order` (one stretch fills its
//!   levels in the order the price meets them), `Terms::bound_exits` (an exit belongs to its entry; read them with
//!   `bindings`), `Terms::stacked_closes` (each close its own order). Set per file, `Terms::committed_exits` (a stop gapped at
//!   the open of a reversing entry fills after it) and `Terms::slipped_trails` (a no-offset trail pays the slippage).
//! - An account kept in USD on a symbol quoted in USDT: `Terms::account_usd`, with `USD_RATES` set (set B only).
//!
//! # What was measured, in rows of list-b ("list only" was read off the list by hand and has not run on the engine)
//!
//! The measurements name the verbs as this module's `Desk` spelled them before the language moved (decision 0269's wave 3):
//! `add_lot`, `add_lot_beyond`, `add_lot_beyond_at` and `add_lot_at` are `entry` (market, stop, stop-limit, limit), `drop_*`
//! `close`, `guard` `exit`, a `Chase` a `TacticsTrail`, `free_*` `order`, `pull` and `pull_all` `cancel` and `cancel_all`,
//! `join_group` `.group`, `held` `position`, `Terms::lot_cap` `Terms::pyramiding`, and `Fee` `vike_model::Commission`.
//!
//! - A market order fills at the next open, in `add_lot` and `drop_*`, with slippage added by the engine (all 12 strategies).
//! - A bar is walked `[O]`, `[O→near]`, `[near→far]`, `[far→C]` (brackets and stops: 366, 728, 757, 325, 671, 1244).
//! - The queue is market orders first, then buys before sells, then placement order within a side, and a replaced order keeps
//!   its slot. A buy and a sell both firing on `[O]` go buy first, whichever was placed first (363 of 363 in both lists; 174 had
//!   the sell placed first). Two sells in one sub-bar go oldest first on list-b, nearest first on list-a alone (3 pairs, the older
//!   also the farther and larger, on 843; 3 bars on another list; 2 of 757). Every tie measured is between entries: guards queue
//!   the same way here, with no list against it, but nothing separates that.
//! - Stop and limit levels snap by side here, on any verb; a trail with an offset does not snap (671). A level is first cut to
//!   `TICK_DECIMALS_KEPT` decimals of a tick, so one a hair over the grid stays on it when it would round up (778, 732, 1620: 3 rows
//!   stay, 9 snap away). That is a harness rule picked by those 12 rows at ETHUSDT's 0.01 tick, with three other readings fitting
//!   equally well, and its true cause is not established. At any other tick it is an assumption with no reference list behind it.
//! - A stop-limit's TRIGGER snaps as a limit does (down for a buy), not as a stop, through the same cut and guard; its limit
//!   snaps as a limit. The engine then fills it by its own stop-limit law, unchanged: at the trigger on this list, which never
//!   gapped past it. 471 of 471 on list-b with this rule, 0 of 471 with the trigger snapped as a stop (each entry one tick
//!   high). No bar of that list has a high between the two snaps, so nothing separates this rule from a law that triggers on
//!   the level rounded up and fills on it rounded down; this harness rule is taken because it leaves the engine's law alone.
//!   List-a prefers the up-snap (469 of 471 by time and price). The limit, 0.28 ATR above the trigger, never bound, so a fill
//!   the limit caps is unmeasured, and the sell side is the mirror, assumed: no list has a sell stop-limit.
//! - Two exits of one entry filled in one bar are written in the order they filled (114 of the 115 pairs at different prices,
//!   24 + 89 + 1, on three lists). The other pair writes the exit issued first ahead of the one the walk reached first, on list-b only,
//!   and no other pair separates the two readings, so that strategy is not a row (its file, never a row, was deleted with the
//!   licence review's other left-outs, 2026-11-10).
//! - An entry's size is fixed when placed. An opposite position adds itself (330 of 843 rows at size 2; 381 of 792 at 2 or more,
//!   up to 10), less any market close of it already issued in this pass (59 of 843 at size 1). A market entry the other way
//!   issued earlier in the pass fills first, so a later entry meets the position it leaves (a stop: 1 pass of
//!   the gallery's `pyramid_deferred_flip_close_all`; a market entry: 1 bar of set B's `fractal_breakout_strategy`, its long
//!   and then its short reversing it). On each fill, a resting entry on the position's own side takes only what it asked (15
//!   of 15; list-a fills the whole size), and otherwise the full size (flat: 327 + 3). The lot cap is judged only when placed,
//!   so a stop placed flat that meets its own side makes a second lot, and one that meets the other side leaves the book flat
//!   (757, 732).
//! - A guard placed with its entry is live in the entry bar, `[O]` included (728). One placed once the lot is held is live from
//!   the next bar (366, 325). It dies with its lots. A guard that fired stays spent while its lots live, however often it is
//!   issued again, and only its own legs are pulled (1244; under `Terms::bound_exits` it is live again once another entry has
//!   filled under their name, as the reference keeps an exit spent per entry fill). A close of a name not held does nothing
//!   (list only: 732).
//! - A trail waits for `after_ticks` past the lot's entry, and activation and exit can share a bar (671, issued once the lot is
//!   held). A trail with no offset leaves at that level itself, so it rests there as a snapped if-touched order whose limit is
//!   that level, ahead of its guard's target, and from the fill when issued with its entry (782, 792, 792, 792). Issued once held
//!   and with this close already at the level, it exits at the next open, whichever side of the level that is, a market order
//!   (372 of 372 on 732; 1 row on the far side). A rule's fractional
//!   tick count rounds up, in the strategy (767 of 767). A target nearer than its trail, and slippage on such an exit, are
//!   unmeasured: no list has either. (Set B's reference slips a no-offset trail like any trailing fill: `Terms::slipped_trails`.)
//! - `Terms::fill_at_close`: market orders fill at this close, stamped with this bar (857), and a resting order placed through
//!   the close fills there too (792). `drop_*_here`: only that order does (366), the rest wait for the next open (drill only).
//! - Percent and per-contract commission are charged on each fill, by quantity to each row (547, 325, 316); per order is flat per
//!   fill (list only: 366). A lot booked on the last bar pays exit commission and no slippage (2).
//! - Sizes are not rounded on list-b (list-a truncates to 4 decimals). A fixed cash size is `cash / slipped(side, close)` with no
//!   commission in it (316; list only: 790). A share of `worth` is `size_for_share(share, slipped(side, close))` (325; per order,
//!   list only: 366); a sell subtracts its slippage (list only: 370 of 421 shorts, 0 if it adds). A resting entry sizes from its
//!   level on the grid, `slipped` for a stop and bare for a limit and for a stop-limit's trigger (686, 146, 471). A buy stop
//!   this close is already through sizes from this close, as a market order does (6 of 146; untested for a limit).
//! - List-b warmed up from 2020 even where `inputs.json` states a range (547, 316, 325); list-a's first trades can differ (1244).
//!
//! - A free order issued for the size held is not reduced by anything: one still resting when a reversal fills adds to the new
//!   position (366, 2240, 1192, both lists, up to size 3). A group that a rule states shrinks by the filled size only cancels here:
//!   on every list that states it the members are the same size, so a fill takes the other to nothing, and no list can tell the
//!   two apart (366, 2240, 1192).
//! - `Plan::after_fill`: after a fill inside a bar the rule runs again, reading that bar's final values, and what it issues works
//!   from the next point of the walk, a market order at that point's price and a limit already through it at that point too (541,
//!   three chains inside one bar). After a fill at a close nothing runs, so its guard waits for the next bar's close (541).
//! - `Terms::fills_a_day` counts fills per UTC day, a reversal being one. The fill that reaches the cap pulls the book, closes what
//!   is held and stops every order until the day ends (1958, 1192; no effect on 732, 302). `Book::count_against_cap` holds what that
//!   close fills at. Its level is the fill's price on the grid, read as the exact decimal: on it 1958 and 1192 are measured
//!   against list-a, the two lists disagreeing at that close (`REFERENCE_LIST_A`). A rule's position limit is the lot cap, and a side the rule may not enter closes what is held (732).
//! - A margin of 50% never refuses an entry worth the whole account (430, reversing on every signal). At 100%, an entry worth more
//!   than the account is refused, not shrunk (79 of 79 and 295 flat days, stated in that strategy, not here), and it is judged twice:
//!   by this close when placed, stated by the rule, and by the open it would fill at, which `Terms::cash_gate` leaves to the engine.
//!   Of 57 Mondays on one list, 25 are taken and 32 refused, 7 by the close alone, 9 by the open alone, 16 by both. List-a splits the same
//!   way on its own worth, 23 taken and 34 refused (list only), and alone has a margin call (1 row).
//! - A lower-timeframe value reads the 15 1-minute bars inside the bar (820, 3116). List-b has no fill outside the bar it carries (0 of
//!   8174 on the three magnifier rules). List-a has 12, 12 and 39, and of its 33, 33 and 104 exits that differ from list-b, 32, 32 and 96
//!   lie on no minute's open or low, so it ran on other data. No one warm-up and trading start fits list-b on all of these (list only):
//!   warmed from 2020 the EMA crosses need a start at 05:30 or later and the RSI cross at 05:00 or earlier, and warmed from 2025-03-31 the
//!   count of up minutes needs 05:15 or later and the RSI cross still 05:00 or earlier. The rows warm from 2020 and start at list-b's first
//!   signal, which fits each. List-a's extra first row of the count comes from warming at 2025-03-31 and starting there (3116 of 3117).
//! - `Terms::magnifier`: the three rules that set it match on this walk (871, 871, 2345), and on the bar's own walk as well, so no row
//!   of list-b needs it or tells the two apart. Each holds one stop at a time, so the order inside a minute cannot show, and of 2239 stops
//!   filled at their level none lies inside the 9353 gaps of 2 ticks or more between one minute's close and the next one's open. A stop
//!   level inside such a gap fills at the next minute's open: drill only. A setting asking for volume-weighted ticks changes no row.
//! - A list may stop short of the data, with a position booked at its last close (1 row each on 820 and 3116).
//!
//! Assumed, no list has tested them: a bar with equal distance goes low first; `avg_cost` follows first-in first-out after a
//! partial close of lots at different prices; two entries in one pass both count as under the cap; `Fee::PerContract` in
//! `size_for_share`; a group whose members differ in size; the order of the walk inside a 1-minute bar, taken from the bar's own.
//!
//! Taken from set B's reference, where set A's lists do not separate them from what the desk did before (set A is identical
//! either way, and with `Terms::price_order` or `Terms::bound_exits` forced on as well): a guard's stop queues ahead of its target,
//! so an open past both fills the stop (10 of 10 such bars on set B's `backtest_template_backtest_terminal`); a trail issued again
//! with other values is a new trail, starting unarmed; an entry of size 0 closes, as above; after a fill inside the window's last
//! bar the rule may issue orders, which fill on that bar's own walk; and an order that triggered beside the fill a rule runs after
//! keeps its name until its own fill, so the rule's cancel cannot reach it, issuing it again queues a new order, and its fill still
//! finds the order that made it; an entry the lot cap refuses takes off the book the one resting under its name; and, with
//! `Terms::bound_exits` only (set A runs without it), the exits of one entry share its quantity in
//! the order they were issued, an exit with nothing left getting no order and an exit with no level holding its share.

use std::cell::{Ref, RefCell};
use std::fs;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use crate::strategies::PlanEntry;
use crate::{BAR_MS, INTERVAL, SYMBOL, Trade};
use vike_analytics::TradingRecord;
use vike_backtest::run::strategy_builder::StrategyBuilder;
use vike_backtest::run::{StrategySource, profile::ProfileBacktest, run_backtest};
use vike_data::{HistoryStore, HistoryStoreMemory, TimestampRange};
use vike_fills::broker_sim::commission;
use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::params::Params;
use vike_model::{
    Commission, CommissionUnit, ContractValue, Fill, Grid, OrderBroker, OrderEvent, OrderFees,
    OrderListLeg, OrderPlacement, Strategy, StreamDelay, TradeClosedRow,
};
use vike_simulator::{ConversionSeries, OrderBrokerSimulator, ShortfallCover};
use vike_tactics::{Rule, RuleDefaults, RuleRun, RuleRunOptions, Tactics, TacticsTrail};
use vike_trade_ledger::{
    LotOpenEntry, TradeClosedAccountTerms, TradeClosedEntry, TradeClosedLedger,
    TradeClosedLedgerOptions, TradeClosedMetrics,
};

pub const DAY_MS: i64 = 86_400_000;

/// What list-b carries: its writer rounds a quantity to 8 decimals and a net result to 6, then strips trailing zeros.
const QUANTITY_DECIMALS: i32 = 8;
const NET_DECIMALS: i32 = 6;

/// A commission stated in the account's own money, which `Terms::account_usd` leaves out of the quote-to-account rate.
fn is_cash(c: Commission) -> bool {
    matches!(c.counted(), Some((_, CommissionUnit::PerContract | CommissionUnit::PerOrder)))
}

/// The `[strategy.params]` lines of the commission `c`: the amount and the unit a backtest reads it in
/// (`crates/vike-model/src/money/costs.rs`'s `CommissionUnit`). `Commission::None` is a commission of nothing.
fn commission_params(c: Commission) -> String {
    let (amount, unit) = c.counted().unwrap_or((0.0, CommissionUnit::Percent));
    format!("commission = {amount:?}\ncommission_unit = \"{}\"", unit.word())
}

/// What one order of `quantity` filled at `price` pays under `c`, charged as the engine charges a fill of it: the engine's
/// fields a commission is (`vike_model::Commission::engine_fee`), the schedule a rule stating it builds, which is what a backtest
/// charges for the strategy's `commission` param that `profile_of` writes (`crates/vike-backtest/src/run/costs.rs`'s
/// `run_costs`), never a flat rate.
pub fn charge(c: Commission, quantity: f64, price: f64) -> f64 {
    let (rate, schedule) = c.engine_fee();
    let order = &mut OrderFees::default();
    commission(schedule, rate, rate, false, quantity, price, ContractValue::default(), None, order)
}

/// One strategy's account and costs, as its header states them. What the run owns reaches it as the keys of the backtest
/// profile `profile_of` writes (`crates/vike-backtest/src/run/profile/engine_config.rs`'s `ProfileEngine`):
/// - `cash` is `engine.cash`, `commission` the strategy's `commission` param in its unit (`commission_params`), `slip_ticks`
///   and `tick` its `slippage` param in ticks of `slippage_tick` (the owner's ruling of 2026-11-18: a run's costs are
///   strategy params), `cash_gate` `engine.cash_gate`, `shortfall_cover` `[engine.margin_call]` and `whole_entry_fee`
///   `engine.whole_entry_fee`;
/// - the in-bar fill rules are `[engine.in_bar]` (`vike_simulator::InBarRules`): `fill_at_close`, `price_order`,
///   `ties_in_placement_order`, `committed_exits` (`committed_stops`) and, with the 1-minute bars as `data.detail_interval`,
///   `magnifier` (each minute on its own path);
/// - the market is `[data]`: `symbol`, `interval` (and `bar_ms`, its length), the window `data.from` and `data.to`.
///
/// The rule's header is `pyramiding` and `commission` (`vike_tactics::RuleDefaults`), and the language's own switches go to its run
/// (`Runner::new`): `fills_a_day`, `bound_exits`, `stacked_closes`, and `slipped_trails` (`vike_tactics::RuleRun::touch_at_market`), for
/// a rule this lab keeps. A gallery row's run takes its row's own options in their place (`Plan::proven`), so the row states
/// them once and the lab proves what a paper mount runs.
/// The one term no key carries is `account_usd`: a conversion rate, which the simulator does not hold (`engine.account_currency`
/// converts nothing; decision 0243 is not ruled), so a USD account's rows are this lab's own booking (`run_split`).
#[derive(Clone, Copy)]
pub struct Terms {
    pub cash: f64,
    pub commission: Commission,
    /// Added against the trader on every fill but a limit fill.
    pub slip_ticks: f64,
    pub tick: f64,
    /// The market the run's bars are of, its bar interval as the datahub names it, and the length of one bar. A harness running
    /// several markets sets them from the row; the defaults are the root's `SYMBOL`, `INTERVAL` and `BAR_MS`.
    pub symbol: &'static str,
    pub interval: &'static str,
    pub bar_ms: i64,
    /// The most lots a side may hold when a new one is asked for. A rule's 0 and 1 both mean one.
    pub pyramiding: usize,
    /// Every market order fills at the close of the bar that placed it, stamped with that bar. A gallery row takes it from its
    /// file's header (`[strategy] fill = "close"`, `gallery_on`), whatever the row's own `Terms` say.
    pub fill_at_close: bool,
    /// The most fills in one UTC day. The fill that reaches it pulls every order and closes what is held on the next sub-bar, and
    /// no order can be placed again until the day ends.
    pub fills_a_day: Option<usize>,
    /// Every bar is walked through its 1-minute bars, each by the path a bar gets, all stamped with the bar. Needs `PlanEntry::minutes`.
    pub magnifier: bool,
    /// The engine drops an opening fill that would take cash below zero at its own price. That is `StrategyEngineConfig::cash_gate`, whose lane
    /// fills on the bar and walks nothing, so it suits only market orders.
    pub cash_gate: bool,
    /// The engine's margin model, `StrategyEngineConfig::shortfall_cover` (`ShortfallCover`'s doc has the rule): after the bar's fills,
    /// a position the account cannot carry at the bar's open or adverse extreme is cut there by a multiple of its shortfall,
    /// and an entry the account cannot margin at its fill is refused. A cut closes lots first in, first out, a row each, and
    /// the rule does not run again after it.
    pub shortfall_cover: Option<ShortfallCover>,
    /// The walk meets the resting orders in the order the price reaches them: each stretch of the next bar's walk is cut at
    /// every level resting when the rule has run, so one stretch never fills two levels at once. At the open, what the open
    /// fills goes market orders first, then entries, then exits, each by its level from the lowest, an exit's legs together
    /// at their lowest level with its stop first. Set B's reference; list-b of set A fills one stretch oldest first.
    pub price_order: bool,
    /// An exit belongs to the entry it was issued for (`vike_tactics::RuleRun::bound_exits` has the rule). Set B's reference.
    pub bound_exits: bool,
    /// Each close is an order of its own, so two closes of one name issued in one pass both fill (language version 6). Without
    /// it the later replaces the earlier.
    pub stacked_closes: bool,
    /// An exit's stop that the next open already gaps through is committed to that open with the size it has: it is no longer
    /// reduce-only, so it still fills after an entry that reversed its lots, and opens that size the other way. A market order
    /// in the queue cancels it as usual, and so does a second leg of the same exit gapped at the same open. Needs
    /// `Terms::price_order` (the entries fill first). The reference's rule for an exit and a reversing stop entry that gap at
    /// one open; set per file, as `stacked_closes` is.
    pub committed_exits: bool,
    /// Under `Terms::price_order`, each leg of an exit ranks by its own level, so the stops of several exits on one level fill in
    /// the order they were placed, not the exit with the lowest target first. Set B's `ultimate_prop_firm_artillery` (three parts
    /// of one short sharing a stop, one fewer row after a margin cut); set per row.
    pub ties_in_placement_order: bool,
    /// The reference charges the slippage on a trailing fill, a no-offset one too. The language rests a no-offset trail as an
    /// if-touched order: with a limit at its level (a limit fill, no slippage) without this, as a market once touched (a taker,
    /// slipped by the engine) with it. Set per file, as `stacked_closes` is: no list of set A has a slipped no-offset trail.
    pub slipped_trails: bool,
    /// A lot's whole entry commission is charged to the first row that closes any of it, where a lot closed in parts shares
    /// it among its rows by quantity (set A's lists, 547, 325, 316). The sum of the rows is the same once the lot is closed;
    /// the rows differ while a part of it is open, which is how a net that leaves a part open on the last bar shows it. The
    /// reference's rule on set B's `macd_strategy` (10% and 50% targets, a part of the entry open at the end: -101.65943
    /// against -101.62654 shared by quantity), `btc_bot` and `dema_atr_strategy_primeautomation` (each off by exactly its open
    /// remainder's entry commission), and no other row is moved by it. Set per file.
    pub whole_entry_fee: bool,
    /// The account is kept in USD on a symbol quoted in USDT: each amount is converted when it is booked, at `Terms::rate` of its
    /// bar. A fill's commission and a closed row's profit at the fill's bar, `Lab::worth`'s open profit at this bar, and a size
    /// from money (`vike_tactics::TacticsSizing`, through `UsdBroker`) divides by this bar's rate. `cash` and a size the rule states or works out itself are
    /// not converted, and neither is a commission stated in cash (`Commission::per_order`, `Commission::per_contract`), which is
    /// in USD already. Set B's `main.rs` has the measurements; the rates come from `USD_RATES`. The owner's named exception
    /// (2026-11-10): such a run's rows are counted from this lab's own ledger, which books them so, and not from the engine's
    /// (`vike_trade_ledger` in `vike_simulator`, which converts nothing), until decision 0243 rules a rate where money is
    /// booked.
    pub account_usd: bool,
    /// The quantity step a size worked out from money floors to (`vike_tactics::RuleRun::qty_step`, which a rule reads through
    /// `vike_tactics::TacticsSizing`). `None` floors nothing, whatever the engine's grid states. Set B's run sets the market's.
    pub quantity_step: Option<f64>,
    /// The market has closed time (the FX week: weekends and holidays), so an input of it misses what the chart's bars miss: a
    /// rule's inputs are judged allowing the window's longest gap between two chart bars (`run_split`). This lab's store holds
    /// no kept instrument row, so the backtest has no trading calendar to tell closed time from a hole by
    /// (`vike_catalog::trading_calendar`). Set B's run sets the market's.
    pub market_closes: bool,
}

/// The USDT-USD pair `Terms::account_usd` converts at, read as the backtest's own conversion reads one
/// (`vike_simulator::ConversionSeries`, decision 0243): one-to-one wherever it has no rate when the backtest's rule marks it
/// (a USDT quote on the dollar ledger, the owner's rulings of 2026-11-16: `vike_backtest::run::data_plan::converts_one_to_one_where_missing`).
/// Set once by a harness that reads it.
pub static USD_RATES: OnceLock<ConversionSeries> = OnceLock::new();

impl Terms {
    pub const PLAIN: Terms = Terms {
        cash: 1_000_000.0,
        commission: Commission::None,
        slip_ticks: 0.0,
        tick: 0.01,
        symbol: SYMBOL,
        interval: INTERVAL,
        bar_ms: BAR_MS,
        pyramiding: 1,
        fill_at_close: false,
        fills_a_day: None,
        magnifier: false,
        cash_gate: false,
        shortfall_cover: None,
        price_order: false,
        bound_exits: false,
        stacked_closes: false,
        committed_exits: false,
        ties_in_placement_order: false,
        slipped_trails: false,
        whole_entry_fee: false,
        account_usd: false,
        quantity_step: None,
        market_closes: false,
    };

    /// What one unit of the quote is worth in the account's money on the bar opening at `timestamp`: 1, or under `Terms::account_usd`
    /// the USDT-USD close of the UTC day before that bar's, one value for every bar of a day: the pair's rate at the day's open
    /// (`vike_simulator::ConversionSeries::rate_at`), which must be the close of exactly that day before, or one-to-one where
    /// the pair has none and its rule says so. A day with no rate on a pair whose rule does not is refused.
    pub fn rate(&self, timestamp: i64) -> f64 {
        if !self.account_usd {
            return 1.0;
        }
        let day = timestamp.div_euclid(DAY_MS) * DAY_MS;
        let pair = USD_RATES.get().expect("a USD account runs with no USDT-USD rates read");
        match pair.rate_at(day) {
            Some(rate) if rate.par || rate.as_of_ms == day => rate.rate.get(),
            _ => panic!("no USDT-USD rate for the UTC day opening at {} ms", day - DAY_MS),
        }
    }
}

/// One strategy's rules: the language's own hooks (`vike_tactics::Rule`'s `on_bar(strategy, bar)`), and the `Terms` its run
/// is made of. `on_bar` is called on EVERY bar fed, so indicators warm up; the order verbs do nothing before trading starts or
/// on the last bar, so a rule never has to ask, except after a fill inside the last bar, whose own walk is still to come. What
/// a rule reads of this lab rather than of the tactics object, it takes from `Lab::current`.
pub trait Plan {
    fn terms(&self) -> Terms;
    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar);
    /// The rule running again right after one of its fills inside `bar`, off unless a rule asks for it. `bar` is whole, and the
    /// rule reads its final values as `on_bar` will (2 chains of 3 rows: a cross only the bar's close makes). An order issued here
    /// rests from the next sub-bar, so a market order fills at the next point of the walk (under `Terms::price_order`, the fill's
    /// own level). Nothing runs after a fill at a close.
    fn after_fill(&mut self, _strategy: &mut Tactics<'_>, _bar: &Bar) {}
    /// The run options a gallery row's rule was proven with (its file header's `[run]`, `vike_strategy_lab::GalleryFile::run`), which its run takes
    /// in place of the terms' four switches; `None` for a rule this lab keeps, whose run takes the terms'.
    fn proven(&self) -> Option<RuleRunOptions> {
        None
    }
    /// The stored series the rule reads beside its bars (`vike_tactics::Rule::inputs`), which its run loads; none by default.
    fn inputs(&self) -> Vec<String> {
        Vec::new()
    }
    /// A gallery row's strategy file as text, which its run's harness reads the header of as it reads a product backtest's
    /// (`vike_backtest::run::StrategySource`): a file marked `runs_on = "backtest"` is served its inputs' whole series.
    /// `None` for a rule this lab keeps.
    fn source(&self) -> Option<&str> {
        None
    }
}

/// One row of a trade list as a list file carries it: the part of a lot one fill closed. A run's rows are counted in
/// `vike_trade_ledger`'s ledger (one lot per entry fill, first in first out, net of both commissions' shares), the
/// engine's (`run_split`); this is what a list is compared on.
#[derive(Clone, Debug)]
pub struct Row {
    pub trade: Trade,
    pub quantity: f64,
    pub net: f64,
}

impl Row {
    /// A closed row as the ledger's statistics list it (`TradeClosedMetrics::trades`).
    fn of_trade(r: &TradeClosedEntry) -> Row {
        Row {
            trade: Trade {
                dir: r.side,
                entry_ms: r.entry_timestamp,
                entry_price: r.entry_price,
                exit_ms: r.exit_timestamp,
                exit_price: r.exit_price,
            },
            quantity: r.qty,
            net: r.net,
        }
    }

    /// A lot booked at the last close (`open_rows`).
    fn of(r: &TradeClosedRow) -> Row {
        Row {
            trade: Trade {
                dir: r.side,
                entry_ms: r.entry_timestamp,
                entry_price: r.entry_price,
                exit_ms: r.exit_timestamp,
                exit_price: r.exit_price,
            },
            quantity: r.qty,
            net: r.net,
        }
    }
}

/// What this lab books beside a run, the books a `Lab` reads: the closed-trade ledger of the rule's own fills, the net it
/// banked, the 1-minute bars, and the account as it stood when the rule's hook started.
struct Books {
    terms: Terms,
    /// One lot per entry fill, each named by the entry it is, first in first out, a row net of its share of both commissions
    /// (`Terms::whole_entry_fee` is its option). The engine folds the same ledger (`engine.closed_trades`), which is what a run
    /// is counted on; this one is what a rule reads (`Lab::done`), what a dump writes, and a USD account's count.
    ledger: TradeClosedLedger,
    /// The closed rows' net, summed as they close.
    banked: f64,
    minutes: Rc<[Bar]>,
    /// `vike_model::OrderBroker::equity` and `vike_model::OrderBroker::price` of the symbol, and the bar's time, at the hook's start.
    equity: f64,
    mark: f64,
    timestamp: i64,
    /// Every fill, `ts side size price fee`, when the run is dumped (`DUMP_DIR`).
    tape: Option<Vec<String>>,
}

impl Books {
    /// Books the fill in the ledger: it closes lots first in, first out, one row per lot, and opens a lot named `lot_name`
    /// with what it has left. `rate` is `Terms::rate` of the fill's bar: each row's profit and, unless it is stated in cash,
    /// the commission are booked at it. `bar` is the fill's bar (`Runner::bar_of`), which the rows count bars by.
    fn fold_fill(&mut self, fill: &Fill, lot_name: &str, rate: f64, bar: usize) {
        let value =
            ContractValue::linear(1.0).in_account(rate).expect("a USDT-USD rate is positive");
        let fee_rate = if is_cash(self.terms.commission) { 1.0 } else { rate };
        for row in self.ledger.apply(fill, lot_name, bar, value, fee_rate) {
            self.banked += row.net;
        }
    }
}

/// What a rule reads of this lab, beside the tactics object, each a read the order language does not give a rule (this file's
/// `//!` lists them): its closed rows (`done`, `banked`), the 1-minute bars inside this bar (`minutes`), and a USD account's
/// money (`worth`, `rate`). A rule takes it with `Lab::current` inside its hook; it reads the books as they stood when
/// the hook started, and nothing it reads moves before a fill.
#[derive(Clone)]
pub struct Lab(Rc<RefCell<Books>>);

thread_local! {
    /// The `Lab` of the rule whose hook is running on this thread (`LabRule`), `None` between hooks.
    static CURRENT: RefCell<Option<Lab>> = const { RefCell::new(None) };
}

impl Lab {
    /// The reads of the run whose rule is running on this thread, for its hook.
    pub fn current() -> Lab {
        CURRENT.with(|c| c.borrow().clone()).expect("a lab read outside a rule's hook")
    }

    fn books(&self) -> Ref<'_, Books> {
        self.0.borrow()
    }

    /// The 1-minute bars inside this bar, oldest first. Empty unless the row sets `PlanEntry::minutes`, and before they are read
    /// from: `data.detail_interval` feeds the engine's fills alone.
    pub fn minutes(&self) -> Ref<'_, [Bar]> {
        Ref::map(self.books(), |b| within(&b.minutes, b.timestamp))
    }

    /// Cash plus the open position at this bar's close. Under `Terms::account_usd` the engine's cash is in the quote, so it is
    /// rebuilt from the books: the capital, the closed rows, less the open lots' entry commission, plus the open profit at this
    /// bar's rate. At a rate of 1 that is the engine's equity, `vike_tactics::Tactics::equity`, which a rule reads instead.
    pub fn worth(&self) -> f64 {
        let b = self.books();
        if !b.terms.account_usd {
            return b.equity;
        }
        let lots = b.ledger.lots();
        let fees: f64 = lots.iter().map(|l| l.entry_fee_left()).sum();
        let open: f64 = lots.iter().map(|l| f64::from(l.side) * l.qty * (b.mark - l.price)).sum();
        b.terms.cash + b.banked - fees + open * b.terms.rate(b.timestamp)
    }

    /// `Terms::rate` of this bar.
    pub fn rate(&self) -> f64 {
        let b = self.books();
        b.terms.rate(b.timestamp)
    }

    /// The net result of every closed row.
    pub fn banked(&self) -> f64 {
        self.books().banked
    }

    /// The closed rows, as the ledger booked them.
    pub fn done(&self) -> Ref<'_, [TradeClosedRow]> {
        Ref::map(self.books(), |b| b.ledger.rows())
    }
}

/// A `Plan` as the language's `vike_tactics::Rule`, with the books a `Lab` reads, current on this thread while its hook runs.
struct LabRule {
    plan: Box<dyn Plan>,
    lab: Lab,
}

impl LabRule {
    /// Notes the account as the hook starts and makes this run's `Lab` current.
    fn enter(&self, strategy: &Tactics<'_>, bar: &Bar) {
        // A USD account's equity is the books' own (`Lab::worth`), which its broker view (`UsdBroker`) reads back.
        let usd = self.lab.books().terms.account_usd;
        let equity = if usd { f64::NAN } else { strategy.equity() };
        let mark = strategy.price();
        let mut b = self.lab.0.borrow_mut();
        (b.equity, b.mark, b.timestamp) = (equity, mark, bar.timestamp);
        CURRENT.with(|c| *c.borrow_mut() = Some(self.lab.clone()));
    }

    fn leave() {
        CURRENT.with(|c| c.borrow_mut().take());
    }
}

impl Rule for LabRule {
    fn defaults(&self) -> RuleDefaults {
        let Terms { pyramiding, commission, .. } = self.lab.books().terms;
        RuleDefaults { pyramiding, fee: commission, ..RuleDefaults::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.enter(strategy, bar);
        self.plan.on_bar(strategy, bar);
        LabRule::leave();
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.enter(strategy, bar);
        self.plan.after_fill(strategy, bar);
        LabRule::leave();
    }

    fn inputs(&self) -> Vec<String> {
        self.plan.inputs()
    }
}

/// The 1-minute bars inside the bar stamped `timestamp`.
fn within(minutes: &[Bar], timestamp: i64) -> &[Bar] {
    let from = minutes.partition_point(|m| m.timestamp < timestamp);
    let to = minutes.partition_point(|m| m.timestamp < timestamp + BAR_MS);
    &minutes[from..to]
}

/// The points of the walk the engine takes through `b` (`InBarRules::path_walk`), each stretch's start and end in turn: its own
/// path, or under `Terms::magnifier` each minute's.
fn walk_points(b: &Bar, minutes: &[Bar], magnifier: bool) -> Vec<f64> {
    let stretches = |[o, near, far, c]: [f64; 4]| [o, o, o, near, near, far, far, c];
    if !magnifier {
        return stretches(b.path_points()).to_vec();
    }
    let inside = within(minutes, b.timestamp);
    assert!(!inside.is_empty(), "no 1-minute bars inside the walked bar at {}", b.timestamp);
    inside.iter().flat_map(|m| stretches(m.path_points())).collect()
}

/// The run's strategy: the rule on the language's wrapper (`vike_tactics::RuleRun`), its fills booked in the lab's books BEFORE the
/// rule hears them, so `Plan::after_fill` reads them updated, and the bars, which the seam does not show while one is walked.
struct Runner {
    run: RuleRun<LabRule>,
    lab: Lab,
    start_ms: i64,
    /// The run's bars, as the profile reads them and the engine steps through them.
    bars: Vec<Bar>,
    /// `Terms::account_usd`: the rule runs on `UsdBroker`.
    usd: bool,
}

/// `--dump DIR`: the run each row is checked on writes `DIR/<row>.txt`, every fill and every row it booked, each f64 as `{:?}`
/// prints it (it reads back to the same bits), so two builds compare row by row with `cmp`.
pub static DUMP_DIR: OnceLock<String> = OnceLock::new();

thread_local! {
    /// The row the next `run_split` on this thread dumps, named by the harness around the run it checks.
    pub static DUMP_ROW: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// The dump of one run: its fills, then its closed rows, then its lots still open booked at the last close.
fn write_dump(
    dir: &str,
    row: &str,
    tape: &[String],
    closed: &[TradeClosedRow],
    open: &[TradeClosedRow],
) {
    let mut text = tape.join("\n");
    text.push_str("\nclosed\n");
    for r in closed {
        text.push_str(&format!("{r:?}\n"));
    }
    text.push_str("open\n");
    for r in open {
        text.push_str(&format!("{r:?}\n"));
    }
    let path = format!("{dir}/{row}.txt");
    fs::write(&path, text).unwrap_or_else(|e| panic!("{path}: {e}"));
}

impl Runner {
    /// `plan` on the market of `lab`'s terms, its verbs live from `start_ms` up to the window's last bar `end_ms`, with the
    /// language's switches the terms state.
    fn new(plan: Box<dyn Plan>, lab: Lab, window: (i64, i64), bars: Vec<Bar>) -> Self {
        let terms = lab.books().terms;
        let switches = plan.proven().unwrap_or(RuleRunOptions {
            bound_exits: terms.bound_exits,
            stacked_closes: terms.stacked_closes,
            fills_a_day: terms.fills_a_day,
            touch_at_market: terms.slipped_trails,
        });
        let rule = LabRule { plan, lab: lab.clone() };
        let run = RuleRun::new(rule, terms.symbol)
            .tick(terms.tick)
            .slippage(terms.slip_ticks * terms.tick)
            .qty_step(terms.quantity_step.unwrap_or(0.0))
            .trading(window.0, window.1)
            .with(switches);
        Runner { run, lab, start_ms: window.0, bars, usd: terms.account_usd }
    }

    /// The bar's fills are booked: the ledger meets the walk the engine took through it and samples the equity at its close.
    fn mark(&mut self, step: usize, bar: &Bar) {
        if bar.timestamp < self.start_ms {
            return;
        }
        let b = &mut *self.lab.0.borrow_mut();
        let walk = walk_points(bar, &b.minutes, b.terms.magnifier);
        let value = ContractValue::linear(1.0)
            .in_account(b.terms.rate(bar.timestamp))
            .expect("a USDT-USD rate is positive");
        b.ledger.mark_bar(b.terms.symbol, step, &walk);
        b.ledger.mark_close(bar.timestamp, |_| Some((bar.close, value)));
    }

    /// The index of the bar `timestamp` falls in, which a row counts its bars by: a fill at a bar's close is booked on the step after
    /// it, stamped with its own bar.
    fn bar_of(&self, timestamp: i64) -> usize {
        self.bars.partition_point(|b| b.timestamp <= timestamp).saturating_sub(1)
    }
}

impl Strategy<OrderBrokerSimulator> for Runner {
    fn inputs(&self) -> Vec<String> {
        self.run.rule().inputs()
    }

    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.mark(broker.index(), bar);
        if self.usd {
            self.run.on_bar(&mut UsdBroker::new(broker, &self.lab), bar);
        } else {
            self.run.on_bar(broker, bar);
        }
    }

    fn on_fill(&mut self, broker: &mut OrderBrokerSimulator, fill: &Fill) {
        let at = self.bar_of(fill.timestamp);
        let lots = self.run.lot_name(fill).to_string();
        {
            let b = &mut *self.lab.0.borrow_mut();
            if let Some(tape) = &mut b.tape {
                let Fill { timestamp, side, size, price, fee, .. } = fill;
                tape.push(format!("{timestamp} {side} {size:?} {price:?} {fee:?}"));
            }
            let rate = b.terms.rate(fill.timestamp);
            b.fold_fill(fill, &lots, rate, at);
        }
        // The bar under way; a fill at a close is stamped with the bar before this step's.
        let under_way = self.bars[self.bar_of(broker.now())].clone();
        if self.usd {
            self.run.on_fill_within(&mut UsdBroker::new(broker, &self.lab), fill, &under_way);
        } else {
            self.run.on_fill_within(broker, fill, &under_way);
        }
    }

    fn on_order_event(&mut self, broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        self.run.on_order_event(broker, event);
    }
}

/// A USD account's view of the engine's broker (`Terms::account_usd`, the owner's named exception), which the rule runs on:
/// every read and verb is the engine's, but the equity is this lab's USD booking (`Lab::worth`) and one unit of the symbol is
/// worth its USDT value at this bar's rate (`Lab::rate`), so a rule sizing from money through the order language
/// (`vike_tactics::TacticsSizing`) sizes in dollars, as this lab's own reads did, and its closed trades are this lab's USD booking's
/// rows (`Lab::done`). The calls name their trait, so no inherent method of the simulator's broker stands in for one.
struct UsdBroker<'a> {
    broker: &'a mut OrderBrokerSimulator,
    lab: &'a Lab,
    /// `Lab::done` as the hook found it, copied on the first read: the books are borrowed again inside the hook.
    done: std::cell::OnceCell<Vec<TradeClosedRow>>,
}

impl<'a> UsdBroker<'a> {
    fn new(broker: &'a mut OrderBrokerSimulator, lab: &'a Lab) -> Self {
        UsdBroker { broker, lab, done: std::cell::OnceCell::new() }
    }
}

impl OrderBroker for UsdBroker<'_> {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        quantity: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        OrderBroker::place(self.broker, symbol, tag, side, quantity, order, reduce_only);
    }
    fn place_exit_leg(
        &mut self,
        symbol: &str,
        exit: &str,
        tag: &str,
        side: i32,
        quantity: f64,
        order: OrderPlacement,
    ) {
        OrderBroker::place_exit_leg(self.broker, symbol, exit, tag, side, quantity, order);
    }
    fn place_at_close(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        quantity: f64,
        reduce_only: bool,
    ) {
        OrderBroker::place_at_close(self.broker, symbol, tag, side, quantity, reduce_only);
    }
    fn place_close_from_fill(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        quantity: f64,
        level: f64,
    ) {
        OrderBroker::place_close_from_fill(self.broker, symbol, tag, side, quantity, level);
    }
    fn rests_reduce_only_while_flat(&self) -> bool {
        OrderBroker::rests_reduce_only_while_flat(self.broker)
    }
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        OrderBroker::place_group(self.broker, symbol, legs);
    }
    fn amend(
        &mut self,
        tag: &str,
        new_quantity: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        OrderBroker::amend(self.broker, tag, new_quantity, new_price, new_trigger);
    }
    fn cancel(&mut self, tag: &str) {
        OrderBroker::cancel(self.broker, tag);
    }
    fn deny(&mut self, symbol: &str, tag: &str, quantity: f64, reason: &str) {
        OrderBroker::deny(self.broker, symbol, tag, quantity, reason);
    }
    fn average_price(&self, symbol: &str) -> f64 {
        OrderBroker::average_price(self.broker, symbol)
    }
    fn position(&self, symbol: &str) -> f64 {
        OrderBroker::position(self.broker, symbol)
    }
    fn price(&self, symbol: &str) -> f64 {
        OrderBroker::price(self.broker, symbol)
    }
    fn equity(&self) -> f64 {
        self.lab.worth()
    }
    fn bars(&self, symbol: &str) -> &[Bar] {
        OrderBroker::bars(self.broker, symbol)
    }
    fn index(&self) -> usize {
        OrderBroker::index(self.broker)
    }
    fn now(&self) -> i64 {
        OrderBroker::now(self.broker)
    }
    fn quote_vwap(&self, symbol: &str, side: i32, quantity: f64) -> Option<f64> {
        OrderBroker::quote_vwap(self.broker, symbol, side, quantity)
    }
    fn depth_within_price(&self, symbol: &str, side: i32, limit_price: f64) -> f64 {
        OrderBroker::depth_within_price(self.broker, symbol, side, limit_price)
    }
    fn data_delay(&self, symbol: &str) -> Option<StreamDelay> {
        OrderBroker::data_delay(self.broker, symbol)
    }
    fn instrument_grid(&self, symbol: &str) -> Option<Grid> {
        OrderBroker::instrument_grid(self.broker, symbol)
    }
    fn contract_value(&self, symbol: &str) -> Option<ContractValue> {
        let value = OrderBroker::contract_value(self.broker, symbol).unwrap_or_default();
        Some(value.in_account(self.lab.rate()).expect("a USDT-USD rate is positive"))
    }
    fn input_bars(&self, name: &str) -> &[Bar] {
        OrderBroker::input_bars(self.broker, name)
    }
    fn input_whole(&self, name: &str) -> Option<&[Bar]> {
        OrderBroker::input_whole(self.broker, name)
    }
    fn closed_trades(&self) -> Option<&[TradeClosedRow]> {
        Some(self.done.get_or_init(|| self.lab.done().to_vec()))
    }
}

/// A rule the strategy gallery took (its strategy file, `vike_strategy_lab::gallery`), as this lab runs it for a row: the
/// file's own code on the row's `Terms`, the commission and the pyramiding its rule's header's, and the run options its file's
/// header states.
struct Gallery {
    rule: Box<dyn Rule + Send>,
    terms: Terms,
    run: RuleRunOptions,
    /// The file's text (`Plan::source`).
    text: String,
}

impl Plan for Gallery {
    fn terms(&self) -> Terms {
        self.terms
    }

    fn source(&self) -> Option<&str> {
        Some(&self.text)
    }

    fn proven(&self) -> Option<RuleRunOptions> {
        Some(self.run)
    }

    fn inputs(&self) -> Vec<String> {
        self.rule.inputs()
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule.on_bar(strategy, bar);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        self.rule.after_fill(strategy, bar);
    }
}

/// The rule of the gallery's strategy file named `name`, fresh, on no params (`vike_strategy_lab::gallery_file`). A name
/// no file has is a broken table.
pub fn from_gallery(name: &str) -> Box<dyn Rule + Send> {
    (gallery_row(name).rule)(&Params::default())
}

/// The gallery's strategy file named `name` (`vike_strategy_lab::gallery_file`). A name no file has is a broken table.
pub fn gallery_row(name: &str) -> vike_strategy_lab::GalleryFile {
    vike_strategy_lab::gallery_file(&strategies_dir(), name)
        .unwrap_or_else(|| panic!("no gallery strategy file {name}.rs"))
}

/// The shipped set's folder, `strategies/` at the repository root: the lab runs in the checkout it was built in.
pub fn strategies_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../strategies")
}

/// The gallery's rule named `name` run on `terms`, its row's: the rule's header states the commission and the pyramiding, and
/// its file's header the run options (`Plan::proven`) and `Terms::fill_at_close` (`[strategy] fill = "close"`, the one place a
/// backtest and a paper mount read it from).
pub fn gallery_on(name: &str, terms: Terms) -> Box<dyn Plan> {
    let row = gallery_row(name);
    let rule = (row.rule)(&Params::default());
    let RuleDefaults { pyramiding, fee: commission, .. } = rule.defaults();
    let terms = Terms { pyramiding, commission, fill_at_close: row.fill_at_close, ..terms };
    let path = strategies_dir().join(row.path);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    Box::new(Gallery { rule, terms, run: row.run, text })
}

/// The venue every series of this lab is the candles of (`bars.rs`).
pub const VENUE: &str = "binance";

/// The lab's bars as the store every run's profile reads (`vike_data::HistoryStoreMemory`): each `(symbol, interval, bars)` under
/// `VENUE`, read once from the datahub by the harness and served to each run over its own window, so a set's hundreds of runs
/// make no read of their own. A series is held under the name a row trades (set A's perpetual `ETHUSDT.P` as `ETHUSDT`, the
/// name its lists and rows carry).
pub fn store_of<'a>(
    series: impl IntoIterator<Item = (&'a str, &'a str, &'a [Bar])>,
) -> Arc<dyn HistoryStore + Send + Sync> {
    let store = HistoryStoreMemory::new();
    for (symbol, interval, bars) in series {
        store
            .append_bars(VENUE, symbol, interval, bars, None)
            .unwrap_or_else(|e| panic!("{symbol} {interval} into the lab's store: {e}"));
    }
    Arc::new(store)
}

/// The window a run of `terms` reads, its first bar `warm_from_ms` and its last `end_ms`: it runs to the END of the last bar, so
/// a detail series (`data.detail_interval`) holds all of that bar's minutes.
fn window(terms: &Terms, warm_from_ms: i64, end_ms: i64) -> (i64, i64) {
    (warm_from_ms, end_ms + terms.bar_ms - 1)
}

/// The bars a run of `terms` over `warm_from_ms ..= end_ms` reads from `store`, as its profile reads them. The window's last
/// bar `end_ms` must exist.
pub fn window_bars(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    terms: &Terms,
    warm_from_ms: i64,
    end_ms: i64,
) -> Vec<Bar> {
    let (from, to) = window(terms, warm_from_ms, end_ms);
    let bars = store
        .load_bars(
            VENUE,
            terms.symbol,
            terms.interval,
            TimestampRange { start: Some(from), end: Some(to) },
        )
        .unwrap_or_else(|e| panic!("{} {}: {e}", terms.symbol, terms.interval));
    assert_eq!(bars.last().map(|b| b.timestamp), Some(end_ms), "the range end bar must exist");
    bars
}

/// The first bar of the input `name` when it is the run's own symbol at a bar length (a one-part name, decision 0271) and
/// `store` holds none of it that opens by `first_ms`: the input starts inside the run's bars. `None` when a bar opens by then,
/// or the name is another series'.
fn late_start(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    terms: &Terms,
    name: &str,
    first_ms: i64,
    end_ms: i64,
) -> Option<i64> {
    if name.contains(':') {
        return None;
    }
    let read =
        |start, end| store.load_bars(VENUE, terms.symbol, name, TimestampRange { start, end }).ok();
    if !read(None, Some(first_ms))?.is_empty() {
        return None;
    }
    read(Some(first_ms), Some(end_ms))?.first().map(|b| b.timestamp)
}

/// The longest span of `first_ms ..= last_ms` the input `name` leaves uncovered when it is another series
/// (`VENUE:SYMBOL:INTERVAL`, decision 0271), measured as the backtest judges an input
/// (`vike_backtest::run::data_plan::input_shortfall`): a share's nights and weekends, and what its vendor's archive lacks, which the
/// reference's platform read across at the series' last value. `None` for the run's own symbol's input (a one-part name).
fn other_gap(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    name: &str,
    first_ms: i64,
    last_ms: i64,
) -> Option<i64> {
    if !name.contains(':') {
        return None;
    }
    let (id, interval_ms) = vike_data::store::series::parse_input_name(name)
        .unwrap_or_else(|e| panic!("the input {name}: {e}"));
    let interval = id.interval.as_deref().expect("a bar series");
    let range = TimestampRange { start: None, end: Some(last_ms) };
    let bars = store
        .load_bars(&id.venue, &id.symbol, interval, range)
        .unwrap_or_else(|e| panic!("the input {name} from the lab's store: {e}"));
    vike_backtest::run::data_plan::input_shortfall(
        &bars,
        interval_ms,
        Some(first_ms),
        Some(last_ms),
    )
    .iter()
    .map(|span| span.len_ms())
    .max()
}

/// The backtest profile a run of `terms` is (`crates/vike-backtest/src/run/profile.rs`'s `ProfileBacktest`), over the bars
/// opening `warm_from_ms ..= end_ms` (`window`), with the in-bar fill rules this lab runs every row on when `in_bar` (`Terms`' doc maps each
/// key). Every number is written as `{:?}` prints it, which reads back to the same bits. Each key the lab always states:
/// - `engine.attach_funding = false`: the lab charges no funding (a perpetual's series would be charged the store's);
/// - the commission is `[strategy.params]` `commission` and `commission_unit`, a commission of nothing `commission = 0.0`,
///   and the slippage, when there is any, `slippage` in ticks of `slippage_tick`;
/// - `engine.closed_trades = true`: the ledger a run is counted on, whatever the key's default;
/// - `engine.margin_mode = "cross"`: a margin account, so a short fills on every series, as the reference's backtests let it;
///   absent, a series whose name says spot (the store's `BTCUSDT`) is judged by the spot law (decision 0276) and its shorts
///   are refused (`crates/vike-backtest/src/run/profile.rs`'s `cash_symbols`);
/// - every bar is walked on its path and the lab's queue is the engine's (`path_walk`, `queue_order`), a trailing order placed
///   from a fill and a reduce-only order are judged as the lab always judged them (`trail_from_fill`, `reduce_only_at_fill`:
///   the reference's backtest-only rule, so an exit issued with its entry rests at once, reduce-only, where live it goes as the
///   entry's OTO child). Under `Terms::cash_gate` the bar is filled whole and walks nothing, so `path_walk` is off (the profile
///   refuses the two together).
/// - no `data.warmup`: that key gates the rule's own hooks, where the lab feeds its indicators every bar from `warm_from_ms`.
pub fn profile_of(terms: &Terms, warm_from_ms: i64, end_ms: i64, in_bar: bool) -> String {
    let Terms { symbol, interval, cash, slip_ticks, tick, cash_gate, whole_entry_fee, .. } = *terms;
    let mut params = vec![commission_params(terms.commission)];
    if slip_ticks != 0.0 {
        params.push(format!(
            "slippage = {slip_ticks:?}\nslippage_unit = \"ticks\"\nslippage_tick = {tick:?}"
        ));
    }
    let mut engine = vec![
        format!("cash = {cash:?}"),
        "attach_funding = false".to_string(),
        "closed_trades = true".to_string(),
        "margin_mode = \"cross\"".to_string(),
        format!("whole_entry_fee = {whole_entry_fee}"),
        format!("cash_gate = {cash_gate}"),
    ];
    if let Some(ShortfallCover { cover, margin_long, margin_short, step }) = terms.shortfall_cover {
        engine.push(format!(
            "margin_call = {{ cover = {cover:?}, margin_long = {margin_long:?}, margin_short = \
             {margin_short:?}, step = {step:?} }}"
        ));
    }
    if in_bar {
        engine.push(format!(
            "in_bar = {{ path_walk = {}, fill_at_close = {}, price_order = {}, queue_order = true, \
             ties_in_placement_order = {}, committed_stops = {}, trail_from_fill = true, reduce_only_at_fill = true }}",
            !cash_gate,
            terms.fill_at_close,
            terms.price_order,
            terms.ties_in_placement_order,
            terms.committed_exits
        ));
    }
    let detail = if terms.magnifier { "detail_interval = \"1m\"\n" } else { "" };
    let (from_ms, to_ms) = window(terms, warm_from_ms, end_ms);
    format!(
        "[data]\nkind = \"bar\"\nvenue = \"{VENUE}\"\nsymbols = [\"{symbol}\"]\ninterval = \"{interval}\"\n\
         from = \"{from_ms}\"\nto = \"{to_ms}\"\n{detail}\n[engine]\n{}\n\n[strategy]\nfile = \
         \"other/strategy_lab.rs\"\n[strategy.params]\n{}\n",
        engine.join("\n"),
        params.join("\n")
    )
}

thread_local! {
    /// The strategy the next `vike_backtest` run on this thread builds: a `StrategyBuilder` is `Send + Sync`, and a rule with
    /// its lab books is neither, so `run_profile` hands it over here, the run being made on this thread.
    static NEXT: RefCell<Option<Box<dyn Strategy<OrderBrokerSimulator>>>> = const { RefCell::new(None) };
}

/// One run through `vike_backtest`: `profile` (TOML, `profile_of`) loaded through the profile's one door
/// (`ProfileBacktest::from_toml_str`, every refusal), `strategy` as its strategy (`ProfileStrategy::builder`, the door for one no
/// strategy file holds), over `store`'s bars (`run_backtest`). `source` is a gallery row's file text (`Plan::source`), whose
/// header the run reads as a product backtest reads its file's.
pub fn run_profile(
    profile: &str,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    strategy: Box<dyn Strategy<OrderBrokerSimulator>>,
    source: Option<&str>,
) -> TradingRecord {
    let mut parsed = ProfileBacktest::from_toml_str(profile)
        .unwrap_or_else(|e| panic!("the lab's profile is refused: {e}\n{profile}"));
    let table = parsed.strategy.as_mut().expect("a lab profile names one [strategy]");
    table.source = source.map(|text| StrategySource::of(text.to_string()));
    NEXT.with(|n| *n.borrow_mut() = Some(strategy));
    table.builder = Some(StrategyBuilder::new(|_| {
        Ok(NEXT.with(|n| n.borrow_mut().take()).expect("one strategy per run, on this thread"))
    }));
    let result = run_backtest(&parsed, Arc::clone(store))
        .unwrap_or_else(|e| panic!("the lab's run is refused: {e}\n{profile}"));
    NEXT.with(|n| assert!(n.borrow().is_none(), "the run built its strategy"));
    result
}

/// What one run books, counted in `vike_trade_ledger`'s ledger.
pub struct Booked {
    /// The closed rows, in closing order: the engine's ledger (`engine.closed_trades`, `TradingRecord::closed_trades`), which
    /// `run_split` checks equal to the lab's own on every row but the entry's name, which the engine does not book. Under
    /// `Terms::account_usd`, the lab's own: the named exception (`Terms::account_usd`).
    pub closed: Vec<TradeClosedEntry>,
    /// The lots still open on the last bar, each booked at that bar's close, paying its exit commission and no slippage.
    pub open: Vec<TradeClosedRow>,
    /// The lab's own ledger of the run, what its rule read (`Lab::done`).
    pub ledger: TradeClosedLedger,
}

fn run(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    plan: Box<dyn Plan>,
    terms: Terms,
    warm_from_ms: i64,
    start_ms: i64,
    end_ms: i64,
) -> Vec<Row> {
    let none: Rc<[Bar]> = Rc::from([]);
    rows_of(&run_split(store, &none, plan, terms, warm_from_ms, start_ms, end_ms))
}

/// A run's rows as a list is compared on: the closed rows, then the lots still open booked at the last close.
fn rows_of(booked: &Booked) -> Vec<Row> {
    booked.closed.iter().map(Row::of_trade).chain(booked.open.iter().map(Row::of)).collect()
}

/// A drill's run: `bars` (the window's last bar the last) and `minutes` as the store, from the first bar.
fn drill_run(bars: &[Bar], minutes: &[Bar], plan: Box<dyn Plan>, terms: Terms) -> Vec<Row> {
    let store = store_of([(terms.symbol, terms.interval, bars), (terms.symbol, "1m", minutes)]);
    let last = bars.last().expect("bars").timestamp;
    let minutes: Rc<[Bar]> = minutes.into();
    rows_of(&run_split(&store, &minutes, plan, terms, 0, 0, last))
}

/// One run of `plan` on `terms` through `vike_backtest` (`profile_of`, `run_profile`): its bars from `store` open
/// `warm_from_ms ..= end_ms` (`window_bars`), the verbs live from `start_ms`; `minutes` is what `Lab::minutes` and a magnifier's
/// walk read.
pub fn run_split(
    store: &Arc<dyn HistoryStore + Send + Sync>,
    minutes: &Rc<[Bar]>,
    plan: Box<dyn Plan>,
    terms: Terms,
    warm_from_ms: i64,
    start_ms: i64,
    end_ms: i64,
) -> Booked {
    let bars = window_bars(store, &terms, warm_from_ms, end_ms);
    assert!(
        minutes.is_empty() || terms.bar_ms == BAR_MS,
        "`within` cuts the minute bars at the root's BAR_MS"
    );
    // Under `Terms::magnifier` every traded bar is walked through its minutes, which must be there: the engine would walk a bar
    // with none on its own path.
    if terms.magnifier {
        for b in bars.iter().filter(|b| b.timestamp >= start_ms) {
            assert!(
                !within(minutes, b.timestamp).is_empty(),
                "no 1-minute bars inside the walked bar at {}",
                b.timestamp
            );
        }
    }
    let last = bars.last().expect("bars").clone();
    let last_bar = bars.len() - 1;
    let dump = DUMP_DIR.get().zip(DUMP_ROW.with(|r| r.borrow_mut().take()));
    let ledger =
        TradeClosedLedger::new(TradeClosedLedgerOptions { whole_entry_fee: terms.whole_entry_fee })
            .with_account(TradeClosedAccountTerms {
                margin_long: terms.shortfall_cover.map_or(0.0, |c| c.margin_long),
                margin_short: terms.shortfall_cover.map_or(0.0, |c| c.margin_short),
                ..TradeClosedAccountTerms::new(terms.cash)
            });
    let lab = Lab(Rc::new(RefCell::new(Books {
        terms,
        ledger,
        banked: 0.0,
        minutes: Rc::clone(minutes),
        equity: 0.0,
        mark: 0.0,
        timestamp: 0,
        tape: dump.is_some().then(Vec::new),
    })));
    let inputs = plan.inputs();
    let source = plan.source().map(str::to_string);
    let first_ms = bars[0].timestamp;
    // A market with closed time (`Terms::market_closes`): the longest stretch between two of the window's bars.
    let closed_ms = terms
        .market_closes
        .then(|| bars.windows(2).map(|w| w[1].timestamp - w[0].timestamp - terms.bar_ms).max())
        .flatten()
        .filter(|&gap| gap > 0);
    let runner = Runner::new(plan, lab.clone(), (start_ms, end_ms), bars);
    let profile = if !inputs.is_empty() {
        // A rule that reads an input has it judged over the window from the first bar on file, not from 0 (`FROM_START`), the
        // same bars either way; and an input that starts later than the bars it warms up on has that span tolerated by its
        // coverage: the 1-minute bars this lab reads (`strategies::FROM_2025_03_31`, all the store holds), whose readers act
        // from `start_ms`, inside the minutes, and in set B a lower series of the row's own symbol its reference read from a
        // later bar (`strategy_lab_b`'s `Listed::inputs_from`), which its rule reads from that bar on, as the reference did. An
        // input of a market with closed time misses what the chart misses, its longest gap tolerated the same way, and an input of
        // another series its own longest gap (`other_gap`).
        let profile = profile_of(&terms, first_ms.max(warm_from_ms), end_ms, true);
        let late = minutes
            .first()
            .map(|m| m.timestamp)
            .into_iter()
            .chain(
                inputs.iter().filter_map(|name| late_start(store, &terms, name, first_ms, end_ms)),
            )
            .filter(|&timestamp| timestamp > first_ms)
            .max();
        let last_ms = end_ms + terms.bar_ms - 1;
        let other_ms =
            inputs.iter().filter_map(|name| other_gap(store, name, first_ms, last_ms)).max();
        match late.map(|late| late - first_ms).max(closed_ms).max(other_ms) {
            Some(gap_ms) => {
                let gap_minutes = (gap_ms + 59_999) / 60_000;
                profile.replacen(
                    "\n\n[engine]",
                    &format!(
                        "\nrequire_coverage = true\nmax_gap = \"{gap_minutes}m\"\non_gap = \"run\"\n\n[engine]"
                    ),
                    1,
                )
            }
            None => profile,
        }
    } else {
        profile_of(&terms, warm_from_ms, end_ms, true)
    };
    let result = run_profile(&profile, store, Box::new(runner), source.as_deref());
    assert_eq!(
        result.simulator().unwrap().intrabar_both_hit,
        0,
        "the coarse lane settled an order"
    );
    let engine = result.closed_trades.expect("the profile states engine.closed_trades");
    let books = &mut *lab.0.borrow_mut();
    let ledger = std::mem::take(&mut books.ledger);
    let rate = terms.rate(last.timestamp);
    let value = ContractValue::linear(1.0).in_account(rate).expect("a USDT-USD rate is positive");
    let conv = if is_cash(terms.commission) { 1.0 } else { rate };
    let exit_fee = |quantity| charge(terms.commission, quantity, last.close) * conv;
    let mine = ledger.metrics(|_| None);
    let open = open_rows(&mine.open_trades, &last, last_bar, value, exit_fee);
    if let (Some((dir, row)), Some(tape)) = (dump, &books.tape) {
        write_dump(dir, &row, tape, ledger.rows(), &open);
    }
    if terms.account_usd {
        return Booked { closed: mine.trades, open, ledger };
    }
    if let Some(why) = differs(&engine, &mine) {
        panic!("the engine's closed-trade ledger and the lab's differ: {why}");
    }
    let open = open_rows(&engine.open_trades, &last, last_bar, value, exit_fee);
    Booked { closed: engine.trades, open, ledger }
}

/// Where the engine's closed-trade ledger and the lab's part, `None` when they agree on every closed row and every lot still
/// open, to the bit, but the entry's name and what depends on the marks (the excursions).
fn differs(engine: &TradeClosedMetrics, lab: &TradeClosedMetrics) -> Option<String> {
    let closed = |t: &TradeClosedEntry| {
        let bits = [t.entry_price, t.exit_price, t.qty, t.net].map(f64::to_bits);
        (t.symbol.clone(), t.side, t.entry_timestamp, t.exit_timestamp, t.bars, bits)
    };
    let open = |o: &LotOpenEntry| {
        let bits = [o.entry_price, o.qty, o.entry_fee].map(f64::to_bits);
        (o.symbol.clone(), o.side, o.entry_timestamp, o.entry_bar, bits)
    };
    let rows = |m: &TradeClosedMetrics| m.trades.iter().map(closed).collect::<Vec<_>>();
    let lots = |m: &TradeClosedMetrics| m.open_trades.iter().map(open).collect::<Vec<_>>();
    let (a, b) = (rows(engine), rows(lab));
    if a != b {
        let at = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
        return Some(format!(
            "{} closed rows against {}, first apart at #{at}: {:?} against {:?}",
            a.len(),
            b.len(),
            a.get(at),
            b.get(at)
        ));
    }
    let (a, b) = (lots(engine), lots(lab));
    (a != b).then(|| format!("open lots {a:?} against {b:?}"))
}

/// The lots still open on `last` (bar `last_bar`), each booked at its close as a list books them: its price profit at `value`,
/// less the entry commission it still carries and `exit_fee` of its quantity, with no slippage.
pub fn open_rows(
    open: &[LotOpenEntry],
    last: &Bar,
    last_bar: usize,
    value: ContractValue,
    exit_fee: impl Fn(f64) -> f64,
) -> Vec<TradeClosedRow> {
    let row = |lot: &LotOpenEntry| {
        let pnl = value.unrealized_pnl(lot.entry_price, last.close, f64::from(lot.side) * lot.qty);
        let exit_fee = exit_fee(lot.qty);
        TradeClosedRow {
            symbol: lot.symbol.clone(),
            tag: lot.tag.clone(),
            side: lot.side,
            entry_timestamp: lot.entry_timestamp,
            entry_bar: lot.entry_bar,
            entry_price: lot.entry_price,
            exit_timestamp: last.timestamp,
            exit_bar: last_bar,
            exit_price: last.close,
            qty: lot.qty,
            pnl,
            commission: lot.entry_fee + exit_fee,
            net: pnl - lot.entry_fee - exit_fee,
        }
    };
    open.iter().map(row).collect()
}

fn read_rows(path: &str) -> Vec<Row> {
    let text = fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    text.lines()
        .skip(1)
        .filter(|l| !l.is_empty())
        .map(|l| {
            let c: Vec<&str> = l.split(',').collect();
            let n = |i: usize| c[i].parse::<f64>().unwrap_or_else(|e| panic!("{l}: {e}"));
            Row {
                trade: Trade {
                    dir: if c[0] == "long" { 1 } else { -1 },
                    entry_ms: c[1].parse().unwrap_or_else(|e| panic!("{l}: {e}")),
                    entry_price: n(2),
                    exit_ms: c[3].parse().unwrap_or_else(|e| panic!("{l}: {e}")),
                    exit_price: n(4),
                },
                quantity: n(5),
                net: n(6),
            }
        })
        .collect()
}

/// A list rounds half a unit of its last decimal either way; the extra hundredth of a unit is room for f64 noise.
fn agrees(mine: f64, list: f64, decimals: i32) -> bool {
    (mine - list).abs() <= 0.51 * 10f64.powi(-decimals)
}

struct Verdict {
    /// Identical in time, price, quantity and net result.
    same: usize,
    /// Identical in time and price, as `Trade::same_as` judges the other strategies.
    path: usize,
    diffs: Vec<String>,
}

fn compare_rows(mine: &[Row], list: &[Row]) -> Verdict {
    let mut v = Verdict { same: 0, path: 0, diffs: Vec::new() };
    for i in 0..mine.len().max(list.len()) {
        let (a, b) = (mine.get(i), list.get(i));
        if let (Some(x), Some(y)) = (a, b)
            && x.trade.same_as(&y.trade)
        {
            v.path += 1;
            if agrees(x.quantity, y.quantity, QUANTITY_DECIMALS)
                && agrees(x.net, y.net, NET_DECIMALS)
            {
                v.same += 1;
                continue;
            }
        }
        if v.diffs.len() < 3 {
            v.diffs.push(format!("  #{i}: vike {a:?}\n       list {b:?}"));
        }
    }
    v
}

fn total(rows: &[Row]) -> f64 {
    rows.iter().map(|r| r.net).sum()
}

/// The table rows whose reference is LIST-A; every other row's is list-b. The two lists disagree on these rows exactly at
/// the fills-a-day cap's close (25 and 14 trades), which fires at the first point of the walk at or past the fill's level.
/// The owner ruled (2026-11-13, P5.1 point 2) that the level is the order snap's EXACT decimal price
/// (`vike_marketdata::TickFraction`, one rule for the book and every order): on it these rows reproduce list-a whole
/// (1958 of 1958, 1192 of 1192), where the old product level, one ulp past the decimal, reproduced list-b.
const REFERENCE_LIST_A: [&str; 2] =
    ["cap_max_intraday_filled_orders_isolate", "bracket_cap_range_pending_stop"];

/// Runs one table row and prints it against both lists. `false` when a list that counts differs. Also hands back what the
/// run booked, as `run_split` does, for the closed-trade check against the reference's own list.
pub fn check(
    dir: &str,
    store: &Arc<dyn HistoryStore + Send + Sync>,
    minutes: &Rc<[Bar]>,
    e: &PlanEntry,
) -> (bool, TradeClosedLedger, Vec<TradeClosedRow>) {
    let first = read_rows(&format!("{dir}/{}.list_a.csv", e.name));
    let second = read_rows(&format!("{dir}/{}.list_b.csv", e.name));
    let plan = (e.build)();
    let terms = plan.terms();
    // An order filling at its own bar's close makes the entry bar the signal bar.
    let signal_gap = if terms.fill_at_close { 0 } else { BAR_MS };
    let start_ms = e.trade_from_ms.unwrap_or(second[0].trade.entry_ms - signal_gap);
    DUMP_ROW.with(|r| *r.borrow_mut() = Some(e.name.to_string()));
    let booked = run_split(store, minutes, plan, terms, e.warm_from_ms, start_ms, e.end_ms);
    let mine = rows_of(&booked);
    // Where the two lists disagree they cannot both match: list-b is the reference, list-a is only shown, except on the
    // rows `REFERENCE_LIST_A` names.
    let reference = if REFERENCE_LIST_A.contains(&e.name) { "list-a" } else { "list-b" };
    let agreeing = compare_rows(&first, &second);
    let lists_agree = agreeing.same == first.len() && first.len() == second.len();
    let mut all_ok = true;
    for (list_name, list) in [("list-a", &first), ("list-b", &second)] {
        let v = compare_rows(&mine, list);
        let ok = v.same == list.len() && mine.len() == list.len();
        let counts = lists_agree || list_name == reference;
        all_ok &= ok || !counts;
        println!(
            "{} {:<32} vs {list_name} vike={} list={} identical={} path={} net vike={:.2} list={:.2}",
            if ok {
                "PASS"
            } else if counts {
                "FAIL"
            } else {
                "INFO"
            },
            e.name,
            mine.len(),
            list.len(),
            v.same,
            v.path,
            total(&mine),
            total(list),
        );
        for d in v.diffs {
            println!("{d}");
        }
    }
    (all_ok, booked.ledger, booked.open)
}

/// The control for the costs: a wrong commission must keep every time and price and lose the net results, or the net
/// comparison above proves nothing.
pub fn cost_control(dir: &str, store: &Arc<dyn HistoryStore + Send + Sync>, e: &PlanEntry) -> bool {
    let list = read_rows(&format!("{dir}/{}.list_b.csv", e.name));
    let plan = (e.build)();
    let terms = Terms { commission: Commission::percent(0.06), ..plan.terms() };
    let start_ms = list[0].trade.entry_ms - BAR_MS;
    let wrong = run(store, plan, terms, e.warm_from_ms, start_ms, e.end_ms);
    let v = compare_rows(&wrong, &list);
    let ok = v.path == list.len() && v.same < list.len();
    println!(
        "{} control: a wrong commission must differ in net only: vike={} list={} identical={} path={}",
        if ok { "PASS" } else { "FAIL" },
        wrong.len(),
        list.len(),
        v.same,
        v.path,
    );
    ok
}

/// Drives every verb no list has reached yet through the real engine, on bars small enough to reason about.
struct Drill;

impl Plan for Drill {
    fn terms(&self) -> Terms {
        Terms { commission: Commission::percent(1.0), ..Terms::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let lab = &Lab::current();
        match bar.timestamp / BAR_MS {
            0 => {
                strategy.entry("A", OrderSide::Long).size(1.0).limit(98.0).group("G");
                strategy.entry("B", OrderSide::Short).size(1.0).limit(105.0).group("G");
            }
            1 => {
                let read = (strategy.position(), strategy.avg_cost());
                assert_eq!(read, (1.0, 98.0), "the limit filled");
                strategy.close("A").size(0.4);
            }
            2 => {
                assert_eq!(lab.done().len(), 1, "a partial close is one row");
                assert!((lab.banked() - 0.008).abs() < 1e-9, "banked {}", lab.banked());
                strategy.entry("C", OrderSide::Short).size(1.0).stop(90.0);
                strategy.cancel("C");
                strategy.exit("X").from("A").take(110.0).stop(90.0);
            }
            3 => strategy.cancel_all(),
            _ => {
                assert_eq!(
                    strategy.lots().len(),
                    1,
                    "group, cancel and cancel-all left only the lot"
                );
                assert!(lab.worth() > 0.0);
            }
        }
    }
}

/// The two doors to a fill at this bar's close. `whole_book` is `Terms::fill_at_close`.
struct CloseDrill {
    whole_book: bool,
}

impl Plan for CloseDrill {
    fn terms(&self) -> Terms {
        if self.whole_book {
            Terms { fill_at_close: true, ..Terms::PLAIN }
        } else {
            Terms { commission: Commission::per_order(1.5), pyramiding: 2, ..Terms::PLAIN }
        }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        match (self.whole_book, bar.timestamp / BAR_MS) {
            (false, 0) => {
                strategy.entry("A", OrderSide::Long).size(1.0);
            }
            (false, 1) => {
                strategy.close_all().here();
                strategy.entry("B", OrderSide::Long).size(2.0);
            }
            (true, 0) => {
                strategy.entry("A", OrderSide::Short).size(1.0).stop(110.0);
            }
            (true, 1) => {
                strategy.close_all();
            }
            _ => {}
        }
    }
}

/// A trail issued in the same pass as its entry, or on the first bar its lot is held.
struct TrailDrill {
    by_ticks: f64,
    with_entry: bool,
}

impl Plan for TrailDrill {
    fn terms(&self) -> Terms {
        Terms::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if bar.timestamp == 0 {
            strategy.entry("A", OrderSide::Long).size(1.0);
        }
        if bar.timestamp == if self.with_entry { 0 } else { BAR_MS } {
            let trail = TacticsTrail { after_ticks: 100.0, by_ticks: self.by_ticks };
            strategy.exit("X").from("A").trail(trail);
        }
    }
}

/// A sell stop and then a buy stop at one level, both through the next open.
struct TieDrill;

impl Plan for TieDrill {
    fn terms(&self) -> Terms {
        Terms::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if bar.timestamp == 0 {
            strategy.entry("S", OrderSide::Short).size(1.0).stop(100.0);
            strategy.entry("B", OrderSide::Long).size(1.0).stop(100.0);
        }
    }
}

/// A stop issued before a close of the long it faces, and with `near` another issued after that close.
struct SizeDrill {
    near: bool,
}

impl Plan for SizeDrill {
    fn terms(&self) -> Terms {
        Terms::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        match bar.timestamp / BAR_MS {
            0 => {
                strategy.entry("A", OrderSide::Long).size(1.0);
            }
            1 => {
                strategy.entry("far", OrderSide::Short).size(1.0).stop(95.0);
                strategy.close("A");
                if self.near {
                    strategy.entry("near", OrderSide::Short).size(1.0).stop(98.0);
                }
            }
            _ => {}
        }
    }
}

/// A free stop bigger than the long it meets, then a free limit that closes what that opened.
struct FreeDrill;

impl Plan for FreeDrill {
    fn terms(&self) -> Terms {
        Terms::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        match bar.timestamp / BAR_MS {
            0 => {
                strategy.entry("A", OrderSide::Long).size(1.0);
                strategy.order("S", OrderSide::Short).size(3.0).stop(95.0);
            }
            3 => {
                strategy.order("B", OrderSide::Long).size(2.0).limit(93.0);
            }
            _ => {}
        }
    }
}

/// A reversal that is the day's second fill, under a cap of two, and an entry refused afterwards.
struct CapDrill;

impl Plan for CapDrill {
    fn terms(&self) -> Terms {
        Terms { fills_a_day: Some(2), ..Terms::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let side = match bar.timestamp / BAR_MS {
            0 | 2 => OrderSide::Long,
            1 => OrderSide::Short,
            _ => return,
        };
        let name = ["A", "B", "C"][(bar.timestamp / BAR_MS) as usize];
        strategy.entry(name, side).size(1.0);
    }
}

/// A target that fires inside a bar, and the rule, run again after it, turning short.
struct RerunDrill;

impl Plan for RerunDrill {
    fn terms(&self) -> Terms {
        Terms::PLAIN
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if bar.timestamp == 0 {
            strategy.entry("A", OrderSide::Long).size(1.0);
            strategy.exit("X").from("A").take(101.0);
        }
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        if bar.timestamp == BAR_MS && strategy.position() == 0.0 {
            strategy.entry("B", OrderSide::Short).size(1.0);
        }
    }
}

/// A long with a stop under it, on a bar walked through its minutes.
struct MinuteDrill;

impl Plan for MinuteDrill {
    fn terms(&self) -> Terms {
        Terms { magnifier: true, ..Terms::PLAIN }
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        let lab = &Lab::current();
        if bar.timestamp == 0 {
            strategy.entry("A", OrderSide::Long).size(1.0);
            strategy.exit("X").from("A").stop(99.0);
        }
        if bar.timestamp == BAR_MS {
            assert_eq!(lab.minutes().len(), 2, "this bar's minutes and no other's");
        }
    }
}

/// What a drill does on bar `k`, and after a fill inside bar `k`.
type Step = fn(&mut Tactics<'_>, i64);

/// A drill written as what the rule does on bar `k`, and after a fill inside bar `k`.
struct Script {
    terms: Terms,
    on_bar: Step,
    after_fill: Step,
}

impl Plan for Script {
    fn terms(&self) -> Terms {
        self.terms
    }

    fn on_bar(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (self.on_bar)(strategy, bar.timestamp / BAR_MS);
    }

    fn after_fill(&mut self, strategy: &mut Tactics<'_>, bar: &Bar) {
        (self.after_fill)(strategy, bar.timestamp / BAR_MS);
    }
}

/// `(entry ms, entry price, exit ms, exit price, quantity, net)` of each row, the net to 1e-9.
type Shape = Vec<(i64, f64, i64, f64, f64, f64)>;

fn shape(rows: &[Row]) -> Shape {
    let row = |r: &Row| {
        let t = &r.trade;
        (
            t.entry_ms,
            t.entry_price,
            t.exit_ms,
            t.exit_price,
            r.quantity,
            (r.net * 1e9).round() / 1e9,
        )
    };
    rows.iter().map(row).collect()
}

/// Runs a `Script` over `bars`, the last of them the window's last bar.
fn script(bars: &[Bar], terms: Terms, on_bar: Step, after_fill: Step) -> Shape {
    let plan = Box::new(Script { terms, on_bar, after_fill });
    shape(&drill_run(bars, &[], plan, terms))
}

/// The drills of the rules set B's reference brought: price order, bound exits, the two orders a rule could crash the run
/// with, the last bar, an entry of size 0, a changed trail, a stop and a target an open passes, and stacked closes.
fn reference_b_drills() {
    use vike_model::OrderSide::{Long, Short};

    fn far_first(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.order("far", Short).size(0.5).stop(94.0);
                s.order("near", Short).size(1.5).stop(96.0);
            }
            _ => {}
        }
    }
    fn near_first(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.order("near", Short).size(1.5).stop(96.0);
                s.order("far", Short).size(0.5).stop(94.0);
            }
            _ => {}
        }
    }
    fn bracket(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.exit("X").from("A").take(102.0).stop(103.0);
            }
            _ => {}
        }
    }
    fn crossing(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.entry("B", Long).size(1.0);
            }
            2 => {
                s.exit("X").from("B").take(103.0);
            }
            3 => {
                let a = s.bindings().iter().map(|b| (b.name.as_str(), b.entry_ms, b.size, b.left));
                assert_eq!(a.collect::<Vec<_>>(), [("A", BAR_MS, 1.0, 1.0)], "B's target took B's");
                s.close("A");
            }
            _ => {}
        }
    }
    fn resting(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("E", Long).size(1.0).stop(105.0);
            s.exit("X").from("E").stop(95.0);
        }
    }
    fn beside(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).size(1.0);
            s.exit("X").from("A").stop(101.0);
        }
    }
    fn again(s: &mut Tactics<'_>, k: i64) {
        if k == 1 {
            s.exit("X").from("A").stop(101.0);
        }
    }
    fn two_guards(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.entry("B", Long).size(1.0);
            }
            2 => {
                s.exit("GB").from("B").stop(96.0);
                s.exit("GA").from("A").stop(95.0);
            }
            _ => {}
        }
    }
    fn enter(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).size(1.0);
        }
    }
    fn stop_on_fill(s: &mut Tactics<'_>, k: i64) {
        if k == 1 {
            s.exit("X").from("A").stop(99.5);
        }
    }
    fn zero(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.entry("Z", Short).size(0.0);
            }
            2 => {
                s.entry("R", Long).size(1.0).stop(110.0);
            }
            3 => {
                s.entry("R", Long).size(0.0).stop(110.0);
            }
            _ => {}
        }
    }
    fn moved(s: &mut Tactics<'_>, k: i64) {
        let trail = |after_ticks| TacticsTrail { after_ticks, by_ticks: 50.0 };
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.exit("X").from("A").trail(trail(100.0));
            }
            2 => {
                s.exit("X").from("A").trail(trail(1000.0));
            }
            _ => {}
        }
    }
    fn halves(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.close("A").size(0.5);
                s.close("A").size(0.5);
            }
            _ => {}
        }
    }
    fn market(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.order("M", Long).size(2.0);
            }
            2 => {
                s.order("N", Short).size(4.0);
            }
            _ => {}
        }
    }
    fn repeated(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
                s.entry("B", Long).size(1.0).stop(110.0);
            }
            1 => {
                s.entry("B", Long).size(1.0).stop(110.0);
            }
            _ => {}
        }
    }
    fn shares(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.exit("half").from("A").size(0.5).stop(95.0);
                s.exit("rest").from("A").stop(99.0);
            }
            _ => {}
        }
    }
    fn first_whole(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.exit("first").from("A").take(120.0);
                s.exit("second").from("A").stop(95.0);
            }
            _ => {}
        }
    }
    fn levelless(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.exit("holder").from("A").size(1.0);
                s.exit("stop").from("A").stop(95.0);
            }
            _ => {}
        }
    }
    fn pending_pair(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("E", Long).size(2.0).stop(105.0);
            s.exit("half").from("E").size(0.5).stop(95.0);
            s.exit("rest").from("E").stop(99.0);
        }
    }
    fn trailed(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).size(1.0);
            s.exit("X").from("A").trail(TacticsTrail { after_ticks: 100.0, by_ticks: 0.0 });
        }
    }
    fn partial(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.close("A").size(0.5);
            }
            2 => {
                s.close_all();
            }
            _ => {}
        }
    }
    fn refilled(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(2.0);
            }
            1 => {
                s.exit("X").from("A").size(1.0).stop(95.0);
                s.entry("A", Long).size(1.0).stop(105.0);
            }
            2 => {
                s.exit("X").from("A").stop(90.0);
            }
            _ => {}
        }
    }
    fn stale(s: &mut Tactics<'_>, k: i64) {
        if k == 0 {
            s.entry("A", Long).size(1.0);
            let trail = TacticsTrail { after_ticks: 50.0, by_ticks: 0.0 };
            s.exit("X").from("A").stop(101.0).trail(trail);
        }
    }
    fn reversing(s: &mut Tactics<'_>, k: i64) {
        match k {
            0 => {
                s.entry("A", Long).size(1.0);
            }
            1 => {
                s.exit("X").from("A").stop(101.0);
                s.entry("B", Short).size(1.0).stop(101.0);
            }
            _ => {}
        }
    }
    fn quiet(_: &mut Tactics<'_>, _: i64) {}

    let flat = |k| drill_bar(k, 100.0, 100.0, 100.0, 100.0);
    let up = |k| drill_bar(k, 101.0, 101.0, 101.0, 101.0);
    let (b, b2, b3, b4, b5) = (BAR_MS, 2 * BAR_MS, 3 * BAR_MS, 4 * BAR_MS, 5 * BAR_MS);
    let plain = Terms::PLAIN;
    let ordered = Terms { price_order: true, ..plain };
    let bound = Terms { bound_exits: true, ..plain };
    let two = Terms { pyramiding: 2, ..plain };

    // Two sell stops under a long of 2, the farther placed first; then the nearer placed first, under an open past both.
    let falling = [flat(0), flat(1), drill_bar(2, 100.0, 100.0, 90.0, 91.0), up(3)];
    assert_eq!(
        script(&falling, ordered, far_first, quiet),
        [(b, 100.0, b2, 96.0, 1.5, -6.0), (b, 100.0, b2, 94.0, 0.5, -3.0)],
        "one stretch fills the level it meets first"
    );
    assert_eq!(
        script(&falling, plain, far_first, quiet),
        [(b, 100.0, b2, 94.0, 0.5, -3.0), (b, 100.0, b2, 96.0, 1.5, -6.0)],
        "without the switch it fills the older first"
    );
    let gapped = [flat(0), flat(1), drill_bar(2, 92.0, 92.0, 90.0, 91.0), up(3)];
    assert_eq!(
        script(&gapped, ordered, near_first, quiet),
        [(b, 100.0, b2, 92.0, 0.5, -4.0), (b, 100.0, b2, 92.0, 1.5, -12.0)],
        "an open past both fills the lower first"
    );

    // A guard's stop above its target, and an open between them: the stop fills, with its slippage.
    let past_both = [flat(0), flat(1), drill_bar(2, 102.5, 102.5, 102.5, 102.5), flat(3)];
    assert_eq!(
        script(&past_both, Terms { slip_ticks: 10.0, ..plain }, bracket, quiet),
        [(b, 100.1, b2, 102.4, 1.0, 2.3)],
        "an open past a stop and a target fills the stop"
    );

    // B's target closes A's lot first in, first out; a close of A then closes what A is still bound to, B's lot.
    let rising = [
        flat(0),
        flat(1),
        up(2),
        drill_bar(3, 101.0, 104.0, 101.0, 104.0),
        drill_bar(4, 105.0, 105.0, 105.0, 105.0),
        drill_bar(5, 106.0, 106.0, 106.0, 106.0),
    ];
    assert_eq!(
        script(&rising, Terms { pyramiding: 2, ..bound }, crossing, quiet),
        [(b, 100.0, b3, 103.0, 1.0, 3.0), (b2, 101.0, b4, 105.0, 1.0, 4.0)],
        "a close of an entry takes what is bound to it, whichever lot first in, first out takes"
    );
    assert_eq!(
        script(&rising, two, crossing, quiet),
        [(b, 100.0, b3, 103.0, 1.0, 3.0), (b2, 101.0, b5, 106.0, 1.0, 5.0)],
        "without the switch A's lot is gone, and its close closes nothing"
    );
    // A stop issued with a resting entry, its level reached before the entry fills: it waits for that fill.
    let dip_then_rise =
        [flat(0), drill_bar(1, 100.0, 106.0, 94.0, 100.0), drill_bar(2, 100.0, 100.0, 90.0, 91.0)];
    assert_eq!(
        script(&dip_then_rise, bound, resting, quiet),
        [(b, 105.0, b2, 95.0, 1.0, -10.0)],
        "the stop works from its entry's fill"
    );
    assert_eq!(
        script(&dip_then_rise, plain, resting, quiet),
        [(b, 105.0, b2, 91.0, 1.0, -14.0)],
        "without the switch the dip spends it before the entry fills"
    );

    // A guard issued again after the fill its own stop triggered beside, at the open.
    let opens_under = [flat(0), drill_bar(1, 100.0, 100.0, 99.0, 99.5), flat(2)];
    assert_eq!(
        script(&opens_under, plain, beside, again),
        [(b, 100.0, b, 100.0, 1.0, 0.0)],
        "the stop that triggered beside the entry still fills, and spends the guard issued again"
    );
    // Two guards of two lots in one stretch: the first fill closes the older lot, and the other guard, its lot gone, still fills.
    let down = [flat(0), flat(1), flat(2), drill_bar(3, 100.0, 100.0, 90.0, 91.0), flat(4)];
    assert_eq!(
        script(&down, two, two_guards, quiet),
        [(b, 100.0, b3, 96.0, 1.0, -4.0), (b2, 100.0, b3, 95.0, 1.0, -5.0)],
        "both stops fill"
    );

    // The window's last bar: what a fill inside it issues fills on its own walk.
    let last = [flat(0), drill_bar(1, 100.0, 100.0, 98.0, 99.0)];
    assert_eq!(
        script(&last, plain, enter, stop_on_fill),
        [(b, 100.0, b, 99.5, 1.0, -0.5)],
        "the stop set after the last bar's entry fills inside it"
    );

    // An entry of size 0 closes the opposite position and opens nothing; with nothing to close it pulls the one resting.
    let zeroed = [flat(0), flat(1), up(2), up(3), drill_bar(4, 101.0, 120.0, 101.0, 101.0), up(5)];
    assert_eq!(
        script(&zeroed, plain, zero, quiet),
        [(b, 100.0, b2, 101.0, 1.0, 1.0)],
        "the close fills, and the stop pulled does not"
    );

    // A trail issued again with another activation is a new trail, not yet active, where one moved in place kept its best.
    let ran_up = [
        flat(0),
        flat(1),
        drill_bar(2, 100.0, 103.0, 100.0, 102.8),
        drill_bar(3, 102.8, 102.8, 101.0, 101.0),
        up(4),
    ];
    assert_eq!(
        script(&ran_up, plain, moved, quiet),
        [(b, 100.0, b4, 101.0, 1.0, 1.0)],
        "the new trail never activates"
    );

    // Two closes of one lot in one pass.
    let closes = [flat(0), flat(1), up(2), up(3)];
    assert_eq!(
        script(&closes, Terms { stacked_closes: true, ..plain }, halves, quiet),
        [
            (b, 100.0, b2, 101.0, 0.5, 0.5),
            (b, 100.0, b2, 101.0, 0.5, 0.5),
            (b, 100.0, b3, 101.0, 1.0, 1.0)
        ],
        "stacked, each close fills"
    );
    assert_eq!(
        script(&closes, plain, halves, quiet),
        [(b, 100.0, b2, 101.0, 0.5, 0.5), (b, 100.0, b3, 101.0, 1.5, 1.5)],
        "otherwise the second replaces the first"
    );

    // A plain market order adds beyond the lot cap and closes first in, first out, opening the other way with the rest.
    let rising = [
        flat(0),
        flat(1),
        up(2),
        drill_bar(3, 102.0, 102.0, 102.0, 102.0),
        drill_bar(4, 103.0, 103.0, 103.0, 103.0),
    ];
    assert_eq!(
        script(&rising, plain, market, quiet),
        [
            (b, 100.0, b3, 102.0, 1.0, 2.0),
            (b2, 101.0, b3, 102.0, 2.0, 2.0),
            (b3, 102.0, b4, 103.0, 1.0, -1.0)
        ],
        "the free market order adds 2 to a cap of 1, then closes 3 and opens 1 short"
    );

    // An entry repeated at the lot cap takes off the book the one resting under its name.
    let through = [
        flat(0),
        flat(1),
        drill_bar(2, 100.0, 112.0, 100.0, 111.0),
        drill_bar(3, 111.0, 111.0, 111.0, 111.0),
    ];
    assert_eq!(
        script(&through, plain, repeated, quiet),
        [(b, 100.0, b3, 111.0, 1.0, 11.0)],
        "the stop placed flat is cancelled by its repeat at the cap, so the rise fills no second lot"
    );

    // The exits of one entry share its quantity in the order they were issued.
    let falling = [
        flat(0),
        flat(1),
        drill_bar(2, 100.0, 100.0, 90.0, 91.0),
        drill_bar(3, 91.0, 91.0, 91.0, 91.0),
    ];
    let ordered_bound = Terms { price_order: true, ..bound };
    let ordered_plain = Terms { price_order: true, ..plain };
    assert_eq!(
        script(&falling, ordered_bound, shares, quiet),
        [(b, 100.0, b2, 99.0, 1.5, -1.5), (b, 100.0, b2, 95.0, 0.5, -2.5)],
        "the second exit takes what the first left, and the first still fills its own half"
    );
    assert_eq!(
        script(&falling, ordered_plain, shares, quiet),
        [(b, 100.0, b2, 99.0, 2.0, -2.0)],
        "without the switch each exit is sized to the whole lot, so the nearer closes it all and the other dies with it"
    );
    assert_eq!(
        script(&falling, bound, first_whole, quiet),
        [(b, 100.0, b3, 91.0, 2.0, -18.0)],
        "an exit issued after one that holds the whole entry gets no order"
    );
    assert_eq!(
        script(&falling, plain, first_whole, quiet),
        [(b, 100.0, b2, 95.0, 2.0, -10.0)],
        "without the switch it fills"
    );
    assert_eq!(
        script(&falling, bound, levelless, quiet),
        [(b, 100.0, b2, 95.0, 1.0, -5.0), (b, 100.0, b3, 91.0, 1.0, -9.0)],
        "an exit with no level holds its share, so the stop issued after it covers only the rest"
    );
    // Two exits issued with an entry still to come: each is sized when the entry fills, the first still holding its half.
    let pending = [
        flat(0),
        drill_bar(1, 100.0, 106.0, 100.0, 105.0),
        drill_bar(2, 105.0, 105.0, 90.0, 91.0),
        drill_bar(3, 91.0, 91.0, 91.0, 91.0),
    ];
    assert_eq!(
        script(&pending, ordered_bound, pending_pair, quiet),
        [(b, 105.0, b2, 99.0, 1.5, -9.0), (b, 105.0, b2, 95.0, 0.5, -5.0)],
        "the exits of an entry that has not filled yet share it as those of one held do"
    );

    // A guard that fired while lots of its name stayed open is spent, unless another entry has filled under the name since:
    // the reference keeps an exit spent per entry fill (a ladder that restates its exit each bar is covered again).
    let swing = [
        flat(0),
        flat(1),
        drill_bar(2, 100.0, 106.0, 94.0, 100.0),
        drill_bar(3, 100.0, 100.0, 85.0, 86.0),
        drill_bar(4, 86.0, 86.0, 86.0, 86.0),
    ];
    let (two_bound, two_plain) =
        (Terms { pyramiding: 2, ..bound }, Terms { pyramiding: 2, ..plain });
    assert_eq!(
        script(&swing, two_bound, refilled, quiet),
        [
            (b, 100.0, b2, 95.0, 1.0, -5.0),
            (b, 100.0, b3, 90.0, 1.0, -10.0),
            (b2, 105.0, b3, 90.0, 1.0, -15.0)
        ],
        "the entry that filled after the stop fired covers the lots again, and the exit issued once more closes them"
    );
    assert_eq!(
        script(&swing, two_plain, refilled, quiet),
        [
            (b, 100.0, b2, 95.0, 1.0, -5.0),
            (b, 100.0, b4, 86.0, 1.0, -14.0),
            (b2, 105.0, b4, 86.0, 1.0, -19.0)
        ],
        "set A's rule: while a lot of its name lives the guard stays spent however often it is issued"
    );

    // A stop an entry's fill is already past, and a trail that the stretch after the fill goes on to touch: the stop fills first.
    let through =
        [flat(0), drill_bar(1, 100.0, 101.5, 98.0, 99.0), drill_bar(2, 99.0, 99.0, 99.0, 99.0)];
    assert_eq!(
        script(&through, bound, stale, quiet),
        [(b, 100.0, b, 100.0, 1.0, 0.0)],
        "the stop the fill is past goes ahead of the trail the same stretch reaches"
    );

    // A lot closed in two parts: its entry commission shared by quantity, or whole on the first row.
    let parts = [
        flat(0),
        flat(1),
        up(2),
        drill_bar(3, 102.0, 102.0, 102.0, 102.0),
        drill_bar(4, 102.0, 102.0, 102.0, 102.0),
    ];
    let fee = Terms { commission: Commission::percent(1.0), ..plain };
    assert_eq!(
        script(&parts, fee, partial, quiet),
        [(b, 100.0, b2, 101.0, 0.5, -0.505), (b, 100.0, b3, 102.0, 1.5, -0.03)],
        "by quantity"
    );
    assert_eq!(
        script(&parts, Terms { whole_entry_fee: true, ..fee }, partial, quiet),
        [(b, 100.0, b2, 101.0, 0.5, -2.005), (b, 100.0, b3, 102.0, 1.5, 1.47)],
        "the first row takes the whole entry commission"
    );

    // A no-offset trail fills at its level, and with `Terms::slipped_trails` pays the slippage there too.
    let touched = [
        flat(0),
        drill_bar(1, 100.0, 102.0, 100.0, 101.5),
        drill_bar(2, 101.5, 101.5, 101.5, 101.5),
    ];
    let slip = 10.0 * 0.01;
    let costly = Terms { slip_ticks: 10.0, ..plain };
    assert_eq!(
        script(&touched, costly, trailed, quiet),
        [(b, 100.0 + slip, b, 101.1, 1.0, 1.0)],
        "a limit pays no slippage"
    );
    assert_eq!(
        script(&touched, Terms { slipped_trails: true, ..costly }, trailed, quiet),
        [(b, 100.0 + slip, b, 101.1 - slip, 1.0, 0.9)],
        "the reference slips it as any trailing fill"
    );

    // A stop beyond the market and a reversing stop entry both gap at one open: the entry fills first, then the stop, which
    // opens its size the other way where it would otherwise be dropped.
    let gap_through = [flat(0), flat(1), flat(2), drill_bar(3, 98.0, 98.0, 98.0, 98.0)];
    let committed = Terms { committed_exits: true, ..ordered_plain };
    assert_eq!(
        script(&gap_through, committed, reversing, quiet),
        [
            (b, 100.0, b2, 100.0, 1.0, 0.0),
            (b2, 100.0, b3, 98.0, 1.0, 2.0),
            (b2, 100.0, b3, 98.0, 1.0, 2.0)
        ],
        "the committed stop fills after the reversal, so the short is held twice"
    );
    assert_eq!(
        script(&gap_through, ordered_plain, reversing, quiet),
        [(b, 100.0, b2, 100.0, 1.0, 0.0), (b2, 100.0, b3, 98.0, 1.0, 2.0)],
        "without the switch the stop, reduce-only, is dropped once the reversal has filled"
    );
}

/// A drill's bar `k`.
fn drill_bar(k: i64, open: f64, high: f64, low: f64, close: f64) -> Bar {
    Bar {
        timestamp: k * BAR_MS,
        open,
        high,
        low,
        close,
        volume: 0.0,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

/// The pieces a matching list cannot vouch for on its own.
pub fn self_check() {
    // A run's profile reads back to its terms' bits (`profile_of` writes every number as `{:?}`).
    let costly = Terms { commission: Commission::percent(0.075), slip_ticks: 3.0, ..Terms::PLAIN };
    let loaded = ProfileBacktest::from_toml_str(&profile_of(&costly, 0, BAR_MS, true))
        .expect("a lab profile loads");
    let param = |key: &str| loaded.strategy.as_ref().and_then(|s| s.params.get(key)).cloned();
    assert_eq!(
        param("commission").and_then(|v| v.as_f64()).map(f64::to_bits),
        Some(0.075_f64.to_bits())
    );
    assert_eq!(param("commission_unit").as_ref().and_then(|v| v.as_str()), Some("percent"));
    assert_eq!(param("slippage").and_then(|v| v.as_f64()), Some(3.0));
    assert_eq!(
        param("slippage_tick").and_then(|v| v.as_f64()).map(f64::to_bits),
        Some(costly.tick.to_bits())
    );

    // The levels this lab measured, on the order layer's side-directed snap (`vike_model::snap_stop`, `snap_limit`) the
    // language sends every level through.
    let up = |tick: f64, p: f64| vike_model::snap_stop(1, p, Some(tick));
    let down = |tick: f64, p: f64| vike_model::snap_stop(-1, p, Some(tick));
    let stop = |side: OrderSide, p: f64| vike_model::snap_stop(i32::from(side), p, Some(0.01));
    let limit = |side: OrderSide, p: f64| vike_model::snap_limit(i32::from(side), p, Some(0.01));
    let t = Terms::PLAIN.tick;
    assert_eq!(up(t, 1802.75 * 1.004), 1809.97);
    assert_eq!(down(t, 1802.75 * 0.996), 1795.53);
    assert_eq!(up(t, 1800.0 * 1.005), 1809.0);
    assert_eq!(down(t, 1800.0 * 1.005), 1809.0);
    assert_eq!((stop(OrderSide::Long, 10.001), stop(OrderSide::Short, 10.001)), (10.01, 10.0));
    assert_eq!((limit(OrderSide::Long, 10.001), limit(OrderSide::Short, 10.001)), (10.0, 10.01));
    assert_eq!(limit(OrderSide::Short, 3760.480000607), 3760.48, "a sell target a hair over stays");
    assert_eq!(
        limit(OrderSide::Long, 2513.2199985159),
        2513.21,
        "a buy target just under goes down"
    );
    assert_eq!(limit(OrderSide::Short, 3760.487), 3760.49, "a sell target well inside goes up");
    let micro = 0.000001;
    assert_eq!((up(micro, 0.1234561), down(micro, 0.1234561)), (0.123457, 0.123456));
    assert_eq!(up(micro, 0.12345600001), 0.123456, "a hair over stays at this tick too");
    assert_eq!(up(micro, 0.1234560005), 0.123457, "past the decimals kept it goes up");
    let nano = 0.00000001;
    assert_eq!((up(nano, 0.00001234561), down(nano, 0.00001234561)), (0.00001235, 0.00001234));
    assert_eq!(up(nano, 0.0000123400005), 0.00001234, "a hair over stays at this tick too");
    assert_eq!(up(nano, 0.000012340005), 0.00001235, "past the decimals kept it goes up");
    // `nano` at 1e8 ticks is where an unscaled guard loses exact prices: 32,890 of these 200,001.
    let grids = [
        (t, 100_000..=500_000, 1),
        (micro, 1..=2_000_000, 7),
        (nano, 1..=2_000_000, 7),
        (nano, 100_000_000..=102_000_000, 10),
    ];
    for (tick, ticks, every) in grids {
        for tick_count in ticks.step_by(every) {
            let on_grid = f64::from(tick_count) / (1.0 / tick).round();
            assert_eq!(up(tick, on_grid), on_grid, "an exact grid price stays");
            assert_eq!(down(tick, on_grid), on_grid, "an exact grid price stays");
        }
    }

    let bar = drill_bar;
    // The path the engine walks each bar along (`InBarRules::path_walk`).
    let path = |b: Bar| b.path_points();
    assert_eq!(path(bar(0, 10.0, 12.0, 9.0, 11.0)), [10.0, 9.0, 12.0, 11.0], "the low is nearer");
    assert_eq!(path(bar(0, 10.0, 11.0, 8.0, 9.0)), [10.0, 11.0, 8.0, 9.0], "the high is nearer");

    assert!(agrees(2.21040998, 2.21040998, QUANTITY_DECIMALS));
    assert!(agrees(2.210409975, 2.21040998, QUANTITY_DECIMALS));
    assert!(!agrees(2.21040, 2.21041, QUANTITY_DECIMALS));
    assert!(!agrees(1.0, 1.1, NET_DECIMALS));

    let drill = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 101.0, 97.0, 100.0), // the buy limit fills at 98, which pulls the sell limit it is grouped with
        bar(2, 100.0, 100.0, 100.0, 100.0), // part of the lot closes at the open
        bar(3, 100.0, 106.0, 100.0, 105.0), // the sell limit would have filled, had it stayed
        bar(4, 105.0, 105.0, 105.0, 105.0), // the pulled stop and the guard would fire on the next bar
        bar(5, 105.0, 120.0, 80.0, 105.0),
        bar(6, 105.0, 105.0, 105.0, 105.0),
    ];
    let rows = drill_run(&drill, &[], Box::new(Drill), Drill.terms());
    let got: Vec<(f64, f64, f64)> =
        rows.iter().map(|r| (r.trade.exit_price, r.quantity, r.net)).collect();
    assert_eq!(got.len(), 2, "{got:?}");
    assert_eq!(got[0].0, 100.0);
    assert!((got[0].1 - 0.4).abs() < 1e-9 && (got[0].2 - 0.008).abs() < 1e-9, "{got:?}");
    assert_eq!(got[1].0, 105.0, "the rest is booked at the last close");
    assert!((got[1].1 - 0.6).abs() < 1e-9 && (got[1].2 - 2.982).abs() < 1e-9, "{got:?}");

    let closes = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 102.0, 99.0, 101.0),
        bar(2, 103.0, 103.0, 103.0, 103.0),
        bar(3, 104.0, 104.0, 104.0, 104.0),
        bar(4, 105.0, 105.0, 105.0, 105.0),
    ];
    let one_order = CloseDrill { whole_book: false };
    let terms = one_order.terms();
    assert_eq!(
        shape(&drill_run(&closes, &[], Box::new(one_order), terms)),
        [
            (BAR_MS, 100.0, BAR_MS, 101.0, 1.0, -2.0),
            (2 * BAR_MS, 103.0, 4 * BAR_MS, 105.0, 2.0, 1.0),
        ],
        "the close fills at this close, and the other order waits for the next open"
    );
    let whole_book = CloseDrill { whole_book: true };
    let terms = whole_book.terms();
    assert_eq!(
        shape(&drill_run(&closes, &[], Box::new(whole_book), terms)),
        [(0, 100.0, BAR_MS, 101.0, 1.0, -1.0)],
        "a stop placed through the close fills at that close, stamped with its own bar"
    );
    let trailed = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 102.0, 99.9, 100.5),
        bar(2, 100.5, 100.5, 100.5, 100.5),
    ];
    let trail = |bars: &[Bar], by_ticks, with_entry| {
        let plan = TrailDrill { by_ticks, with_entry };
        shape(&drill_run(bars, &[], Box::new(plan), Terms::PLAIN))
    };
    assert_eq!(
        trail(&trailed, 50.0, true),
        [(BAR_MS, 100.0, BAR_MS, 101.5, 1.0, 1.5)],
        "it activates at 101 and fires 0.5 under the high, all in the entry bar"
    );
    assert_eq!(
        trail(&trailed, 0.0, true),
        [(BAR_MS, 100.0, BAR_MS, 101.0, 1.0, 1.0)],
        "with no offset it leaves at 101 itself, in the entry bar"
    );
    let trailed_later = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.5, 99.9, 100.5),
        bar(2, 100.5, 102.0, 100.4, 100.5),
        bar(3, 100.5, 100.5, 100.5, 100.5),
    ];
    assert_eq!(
        trail(&trailed_later, 0.0, false),
        [(BAR_MS, 100.0, 2 * BAR_MS, 101.0, 1.0, 1.0)],
        "issued once held it rests from the next bar, at 101 itself"
    );
    let already_reached = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 101.5, 99.9, 101.2),
        bar(2, 100.7, 100.9, 100.6, 100.8),
        bar(3, 100.8, 100.8, 100.8, 100.8),
    ];
    assert_eq!(
        trail(&already_reached, 0.0, false),
        [(BAR_MS, 100.0, 2 * BAR_MS, 100.7, 1.0, 0.7)],
        "a level this close has reached fires at the next open, under it or not"
    );

    let free = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 100.0, 100.0, 94.0, 94.0),
        bar(3, 94.0, 94.0, 94.0, 94.0),
        bar(4, 94.0, 94.0, 92.0, 92.0),
        bar(5, 92.0, 92.0, 92.0, 92.0),
    ];
    assert_eq!(
        shape(&drill_run(&free, &[], Box::new(FreeDrill), Terms::PLAIN)),
        [
            (BAR_MS, 100.0, 2 * BAR_MS, 95.0, 1.0, -5.0),
            (2 * BAR_MS, 95.0, 4 * BAR_MS, 93.0, 2.0, 4.0)
        ],
        "the stop closes the long and opens with the rest, and the limit closes that"
    );
    let capped = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 101.0, 101.0, 101.0, 101.0),
        bar(3, 102.0, 102.0, 102.0, 102.0),
        bar(4, 102.0, 102.0, 102.0, 102.0),
    ];
    let terms = CapDrill.terms();
    assert_eq!(
        shape(&drill_run(&capped, &[], Box::new(CapDrill), terms)),
        [
            (BAR_MS, 100.0, 2 * BAR_MS, 101.0, 1.0, 1.0),
            (2 * BAR_MS, 101.0, 2 * BAR_MS, 101.0, 1.0, 0.0)
        ],
        "the fill that reaches the cap closes at its own price, and no entry is taken after it"
    );
    let rerun = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 102.0, 99.5, 101.5),
        bar(2, 101.5, 101.5, 101.5, 101.5),
    ];
    assert_eq!(
        shape(&drill_run(&rerun, &[], Box::new(RerunDrill), Terms::PLAIN)),
        [(BAR_MS, 100.0, BAR_MS, 101.0, 1.0, 1.0), (BAR_MS, 102.0, 2 * BAR_MS, 101.5, 1.0, 0.5)],
        "the order issued after the target fills at the next point of the walk"
    );
    // The bar's own walk would leave at 99 itself. Its minutes jump over 99, so the stop waits for the second minute's open.
    let gapped = [bar(0, 100.0, 100.0, 100.0, 100.0), bar(1, 100.0, 100.0, 98.5, 98.7)];
    let minute = |timestamp, open: f64, close: f64| Bar {
        timestamp,
        high: open.max(close),
        low: open.min(close),
        ..bar(0, open, 0.0, 0.0, close)
    };
    let inside =
        [minute(0, 100.0, 100.0), minute(BAR_MS, 100.0, 99.5), minute(BAR_MS + 60_000, 98.8, 98.7)];
    let terms = MinuteDrill.terms();
    assert_eq!(
        shape(&drill_run(&gapped, &inside, Box::new(MinuteDrill), terms)),
        [(BAR_MS, 100.0, BAR_MS, 98.8, 1.0, -1.2)],
        "a stop level between two minutes fills at the later one's open, and the fill carries the bar's time"
    );

    let tie = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 100.0, 100.0, 100.0, 100.0),
    ];
    let rows = drill_run(&tie, &[], Box::new(TieDrill), Terms::PLAIN);
    assert_eq!(
        rows.iter().map(|r| r.trade.dir).collect::<Vec<_>>(),
        [1],
        "the buy stop fills first, though placed second"
    );

    let sized = [
        bar(0, 100.0, 100.0, 100.0, 100.0),
        bar(1, 100.0, 100.0, 100.0, 100.0),
        bar(2, 100.0, 100.0, 97.0, 97.0),
        bar(3, 97.0, 97.0, 94.0, 94.0),
        bar(4, 94.0, 94.0, 94.0, 94.0),
    ];
    let size_drill =
        |near| shape(&drill_run(&sized, &[], Box::new(SizeDrill { near }), Terms::PLAIN));
    assert_eq!(
        size_drill(false),
        [
            (BAR_MS, 100.0, 2 * BAR_MS, 100.0, 1.0, 0.0),
            (3 * BAR_MS, 95.0, 4 * BAR_MS, 94.0, 2.0, 2.0)
        ],
        "issued before the close it keeps the size it added, when it fills flat"
    );
    assert_eq!(
        size_drill(true),
        [
            (BAR_MS, 100.0, 2 * BAR_MS, 100.0, 1.0, 0.0),
            (2 * BAR_MS, 98.0, 4 * BAR_MS, 94.0, 1.0, 4.0),
            (3 * BAR_MS, 95.0, 4 * BAR_MS, 94.0, 1.0, 1.0),
        ],
        "issued after the close it adds nothing, and filling into its own side the first takes only what it asked"
    );

    let fill = |side, size, price, fee, timestamp| Fill {
        side,
        size,
        price,
        fee,
        timestamp,
        is_maker: false,
        symbol: String::new(),
        tag: None,
    };
    let booked = || Books {
        terms: Terms::PLAIN,
        ledger: TradeClosedLedger::default(),
        banked: 0.0,
        minutes: Rc::from([]),
        equity: 0.0,
        mark: 0.0,
        timestamp: 0,
        tape: None,
    };
    let mut book = booked();
    book.fold_fill(&fill(1, 1.0, 10.0, 0.1, 1), "A", 1.0, 0);
    book.fold_fill(&fill(1, 1.0, 11.0, 0.1, 1), "B", 1.0, 0);
    book.fold_fill(&fill(-1, 3.0, 12.0, 0.3, 2), "", 1.0, 1);
    let lots: Vec<(f64, f64)> = book.ledger.rows().iter().map(|r| (r.entry_price, r.net)).collect();
    assert_eq!(lots.len(), 2, "two lots close first in, first out");
    assert_eq!(lots[0].0, 10.0);
    assert!((lots[0].1 - 1.8).abs() < 1e-9 && (lots[1].1 - 0.8).abs() < 1e-9, "{lots:?}");
    let open = &book.ledger.lots()[0];
    assert_eq!((open.side, open.qty), (-1, 1.0), "a reversal opens a lot");
    assert!((book.banked - 2.6).abs() < 1e-9, "banked sums the rows: {}", book.banked);

    let mut usd = booked();
    usd.fold_fill(&fill(1, 1.0, 10.0, 0.1, 1), "A", 2.0, 0);
    usd.fold_fill(&fill(-1, 1.0, 12.0, 0.1, 2), "", 0.5, 1);
    let net = usd.ledger.rows()[0].net;
    assert!(
        (net - 0.75).abs() < 1e-9,
        "the profit and the exit commission at the exit's rate, the entry commission at the entry's: {net}"
    );

    reference_b_drills();
}
