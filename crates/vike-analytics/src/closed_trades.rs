//! Closed-trade accounting: a run's FILLS folded into a list of closed trades the way the reference
//! report counts them, and the trades-analysis statistics over that list in three columns (all,
//! long, short).
//!
//! # The rule
//!
//! - **One lot per entry fill.** The part of a fill that opens or adds to a position is a lot of
//!   its own, so a pyramided position is several lots, each with its own price, time and
//!   commission.
//! - **Exits close the oldest lots first** (first in, first out), within the fill's symbol. Each
//!   (closing fill × lot) pair the fill reaches is one ROW: the part of that lot this fill closed.
//! - **A row is net of its share of BOTH commissions**: the lot's entry commission by quantity and
//!   the closing fill's by quantity. [`TradeClosedLedgerOptions::whole_entry_fee`] charges instead whatever
//!   entry commission a lot still carries to the first row that closes any of it. The two agree
//!   once a lot is closed; they differ while part of it is open.
//! - **A flip closes, then opens**: a fill larger than the position closes every lot and opens a
//!   lot with the rest, carrying the rest's share of the fill's commission.
//! - **Lots still open are kept apart** ([`TradeClosedLedger::lots`]): no statistic counts them as
//!   trades, [`TradeClosedStatistics::open_trades`] and [`TradeClosedStatistics::open_pnl`] report them at a
//!   mark, and [`TradeClosedMetrics::open_trades`] lists them, so a reader whose reference books an
//!   open trade at the last bar can do so from the result.
//!
//! The rule is the strategy lab's (set A's `desk.rs`), which matched the reference's trade lists
//! row by row, moved here so the lab books through it and a report can.
//!
//! # Bars, excursions and the account
//!
//! A caller that also hands the ledger each bar ([`TradeClosedLedger::mark_bar`], the bar's walk:
//! open, the extreme nearer the open, the other, the close) and each close
//! ([`TradeClosedLedger::mark_close`]) gets each trade's excursions and the account-wide figures
//! ([`TradeClosedAccountStatistics`], after [`TradeClosedLedger::with_account`]). A held lot meets every bar
//! it is held through; its entry bar from its entry price on, and the row's exit bar up to its exit
//! price (a trade closed at the open meets the open alone): the reference's rule. An excursion is
//! the price P&L at the extreme less the entry commission the row carries, as the reference's list
//! of trades prints it. Without marks a trade's excursions are its entry and exit prices, and the
//! intrabar and monthly figures have nothing to measure (zero).
//!
//! # What the reference was measured to do
//!
//! Five of its reports (2026-11-10: three strategies on three markets, 2026-01-01 to the last bar
//! closed; closed-trade counts all equal to this ledger's, the closed net to the cent on three)
//! settled what its documentation leaves open or states otherwise. Each is the ledger's reading:
//!
//! - **Max drawdown** (money, and a percent of the peak it fell from): the highest CLOSED-TRADE
//!   equity before a bar (the initial capital at first) against the equity at that bar's adverse
//!   extreme (the closed rows' net and the open lots at the low of a long, the high of a short, less
//!   their entry commission). To the cent on the three reports whose closed trades match (26,925.77
//!   and 26.54%, 20,523.71 and 20.52%, 0.89), within 4.15 on the two that differ by a trade. The
//!   highest equity at a bar's close as the peak gave 26,927.15 and 0.93; the help page's per-trade
//!   reading (each trade from the closed-trade equity at its entry) 16,211.65 for the 20,523.71.
//!   The percent of the initial capital is kept beside it.
//! - **Drawdown and run-up, growth and decline** (max, average, average days, the run-up in
//!   progress): on the CLOSED-TRADE equity, the capital and the closed rows' net after each row,
//!   starting at the first row; a drawdown's days run from its peak to its lowest point, a run-up's
//!   from its trough to its peak; a fill whose rows dip and recover inside it opens no drawdown, and
//!   a row inside a fill can be a drawdown's lowest point. All seven figures match on the two
//!   reports whose closed trades match (a pyramiding one among them: 25.57% / 8.73% / 71 days,
//!   6.51% / 2.16% / 6.51% / 23 days), the day counts on all five (the reference prints whole days,
//!   rounded down); the bar-close equity gives none of them.
//! - **CAGR**: the closed net profit's return (not the one with open trades), over the calendar days
//!   from the first bar to the last, in years of 365 days (-46.45% where the open trades' return
//!   gives -45.85%).
//! - **Outperformance**: the return with open trades less buy and hold's, in percentage points.
//! - **Buy and hold**: from the first entry's fill price to the last price. The four reports that
//!   print its return imply, at their capture times, each market's own minute price, a long first
//!   entry and a short one alike.
//! - **Buy and hold on the chart** (money): at the last closed row's exit price when nothing is open
//!   (the closed-trade equity's last point): -510.95 and -41,149.60 to the cent on the two reports
//!   that end flat (the first beside a return of -50.75% at the capture's price, -507.50).
//! - **Expectancy %**: the mean of the rows' net percents (each over its entry's value): -0.52%,
//!   -0.34%, -0.27% on the three reports that print it.
//! - **Outliers**: the rows whose net percent lies more than two deviations (`N - 1`) from the mean;
//!   their net (and its share of the capital), and **average profit % / average loss %** are the
//!   winning and losing rows' mean net percent without them: -29,872.98, 1.34%, -1.99% and 0.84,
//!   1.64%, -1.16% exactly on the two reports whose trades match; on the third, four rows sit within
//!   0.01% of the cut and the reference's cut, moved by its one differing trade, leaves them in
//!   (their -329.47 is the whole gap).
//! - **Streaks**: the most winning and losing rows in a row, in closing order (28 / 10, 3 / 21,
//!   26 / 9 exact; 39 / 30 against 40 / 30 where a trade differs). Their averages are hidden on the
//!   captures: the mean run length is the plain reading.
//! - **Average margin used**: the margin at each bar's close over the bars from the first entry's
//!   to the last (10,533.68 exactly; over every bar of the window it is 10,494.45), at the engine's
//!   margin model's rates (`vike_simulator::ShortfallCover`), none without one, as on the reports whose
//!   scripts state none; **margin efficiency** the closed net over it (-1.86).
//! - **A trade's run-up and drawdown** (the list of trades): the walk rule above, less the row's entry
//!   commission. Every trade of 193 of the 221 strategies whose rows match the reference's own lists
//!   carries the list's figures to the cent.
//!
//! Not on those reports, so the documented reading stands: the monthly Sharpe and Sortino
//! ([`TradeClosedAccountRatio::DOCUMENTED`]) and the intrabar run-up (the drawdown's mirror). Whether the open
//! trades' P&L carries their entry commission (it does here) the reports cannot tell: on the one
//! with open trades the price moved more during the capture than the commission is.
//!
//! # How it differs from the trades every report already has
//!
//! `vike_model::TradeFold` (the engine's [`crate::BacktestResult::trades`]) folds the NET position:
//! adds average into one entry price, a partial close books its part at that average, and a trade's
//! `pnl` is GROSS of fees. This ledger keeps every entry apart and nets every row. Neither replaces
//! the other: the report's existing metrics are the net-position fold's and do not move; this mode
//! is a separate block ([`TradeClosedMetrics`]), present only when a run arms it.
//!
//! # Money
//!
//! Price P&L goes through `vike_model::ContractValue` (`unrealized_pnl`, then `in_settle` at the
//! exit price), as `TradeFold::apply_value` books a closed trade, so a contract's kind and unit
//! cannot be dropped here. The caller states what one unit is worth in the account's money
//! (`value`, already `in_account`) and what one unit of the fill's fee is worth (`fee_rate`),
//! because the two can differ: a commission stated in cash is in the account's money already.

use serde::{Deserialize, Serialize};
use vike_model::{
    ContractValue, Fill, LOT_DUST, LotClose, TradeClosedRow, close_lots_fifo, py_sum,
};

mod account;

pub use account::{
    DEFAULT_RISK_FREE_RATE, TradeClosedAccountDivisor, TradeClosedAccountMonthlyRate,
    TradeClosedAccountRatio, TradeClosedAccountStatistics, TradeClosedAccountTerms,
};

/// The most points [`TradeClosedMetrics::buy_hold_curve`] keeps: a line to draw, not a series to read.
pub const BUY_HOLD_POINTS: usize = 512;

/// The booking switches. Both readings exist in the reference's own lists, so each is a choice, off
/// by default.
///
/// The lab's other per-reference switch, `stacked_closes`, has no twin here: it decides how many
/// ORDERS a rule's closes become, and this ledger books whatever fills those orders made, one row
/// per fill and lot either way.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TradeClosedLedgerOptions {
    /// The whole entry commission a lot still carries goes to the first row that closes any of it,
    /// instead of a share by quantity to each row.
    pub whole_entry_fee: bool,
}

/// What arms closed-trade accounting on a run: the booking switches and the yearly risk-free rate
/// the monthly ratios subtract. The run's starting cash is the engine's, so it is not here.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TradeClosedTerms {
    pub options: TradeClosedLedgerOptions,
    /// A yearly fraction; [`DEFAULT_RISK_FREE_RATE`] by default.
    pub risk_free_rate: f64,
}

impl Default for TradeClosedTerms {
    fn default() -> Self {
        Self {
            options: TradeClosedLedgerOptions::default(),
            risk_free_rate: DEFAULT_RISK_FREE_RATE,
        }
    }
}

/// An entry still open: one entry fill, or what first-in first-out has left of it.
#[derive(Debug, Clone, PartialEq)]
pub struct OpenLot {
    pub symbol: String,
    /// The caller's name for the entry (the list of trades' signal), empty when it has none.
    pub tag: String,
    /// `+1` long, `-1` short.
    pub side: i32,
    pub entry_timestamp: i64,
    /// The caller's bar index of the entry fill.
    pub entry_bar: usize,
    pub price: f64,
    pub qty: f64,
    /// The entry commission per unit, in the account's money; zero once
    /// [`TradeClosedLedgerOptions::whole_entry_fee`] charged it.
    fee_unit: f64,
    /// The lowest and highest prices met while held (the entry price at first).
    met: (f64, f64),
    /// The lowest and highest prices met in the last bar marked (the entry price before one is).
    bar_met: (f64, f64),
    value: ContractValue,
}

impl OpenLot {
    /// The entry commission this lot still carries: what a row closing all of it would be charged.
    pub fn entry_fee_left(&self) -> f64 {
        self.fee_unit * self.qty
    }

    /// The entry commission per unit the lot carries, in the account's money: what each unit a row closes
    /// is charged of it ([`Self::entry_fee_left`] is this times the quantity left).
    pub fn entry_fee_per_unit(&self) -> f64 {
        self.fee_unit
    }

    /// What one unit of the lot's contract is worth in the account's money, as its entry fill was booked.
    pub fn value(&self) -> ContractValue {
        self.value
    }

    /// A lot as a ledger held it, to RESUME that ledger ([`TradeClosedLedger::resumed`]): the fields this type
    /// shows, with [`Self::entry_fee_per_unit`] and [`Self::value`]. Its excursions start at its entry price
    /// (a live ledger takes no marks).
    pub fn resumed(
        symbol: String,
        tag: String,
        side: i32,
        (entry_ts, entry_bar): (i64, usize),
        price: f64,
        qty: f64,
        entry_fee_per_unit: f64,
        value: ContractValue,
    ) -> Self {
        OpenLot {
            symbol,
            tag,
            side,
            entry_timestamp: entry_ts,
            entry_bar,
            price,
            qty,
            fee_unit: entry_fee_per_unit,
            met: (price, price),
            bar_met: (price, price),
            value,
        }
    }
}

impl vike_model::Lot for OpenLot {
    fn side(&self) -> i32 {
        self.side
    }

    fn quantity_mut(&mut self) -> &mut f64 {
        &mut self.qty
    }
}

/// What a closed row keeps beside it for its excursions.
#[derive(Debug, Clone, PartialEq)]
struct RowMet {
    met: (f64, f64),
    entry_fee: f64,
    value: ContractValue,
}

/// The intrabar drawdown and run-up as they run, less the initial capital: the highest and lowest
/// closed-trade equity at a bar's close so far, and the deepest fall from the one and highest rise from
/// the other to a later bar's extreme, each with where it started.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Intrabar {
    peak: f64,
    trough: f64,
    drawdown: f64,
    drawdown_from: f64,
    runup: f64,
    runup_from: f64,
}

/// The index of the first stretch of `walk` that reaches `price`, a one-point walk its point.
fn first_reach(walk: &[f64], price: f64) -> Option<usize> {
    if walk.len() == 1 {
        return (walk[0] == price).then_some(0);
    }
    walk.windows(2).position(|s| s[0].min(s[1]) <= price && price <= s[0].max(s[1]))
}

/// The lowest and highest prices `walk` visits from where it first reaches `from` to where it next
/// reaches `to`; an end that is `None`, or that the walk never reaches, is the walk's own end.
fn walk_range(walk: &[f64], from: Option<f64>, to: Option<f64>) -> (f64, f64) {
    let mut points: Vec<f64> = walk.to_vec();
    if let Some(x) = from
        && let Some(i) = first_reach(&points, x)
    {
        points = std::iter::once(x).chain(points[i + 1..].iter().copied()).collect();
    }
    if let Some(y) = to
        && let Some(i) = first_reach(&points, y)
    {
        points.truncate(i + 1);
        points.push(y);
    }
    let lo = points.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = points.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    (lo, hi)
}

fn widen(met: &mut (f64, f64), (lo, hi): (f64, f64)) {
    met.0 = met.0.min(lo);
    met.1 = met.1.max(hi);
}

/// The fold: open lots, closed rows (`vike_model::TradeClosedRow`, a row's tag the lot's
/// [`OpenLot::tag`]), and the most each side held.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TradeClosedLedger {
    options: TradeClosedLedgerOptions,
    lots: Vec<OpenLot>,
    rows: Vec<TradeClosedRow>,
    /// Beside each of `rows`, in step.
    row_met: Vec<RowMet>,
    /// The rows whose exit bar is still to be marked.
    pending: Vec<usize>,
    /// Per symbol, the last bar [`Self::mark_bar`] marked.
    marked: Vec<(String, usize)>,
    /// The closed rows' cumulative net now.
    cum: f64,
    /// Each closing fill on the closed-trade equity (less the initial capital), in order.
    trade_closes: Vec<account::Close>,
    /// The equity less the initial capital at each [`Self::mark_close`].
    closes: Vec<(i64, f64)>,
    intrabar: Intrabar,
    /// The symbol and price of the first entry, for buy and hold.
    first_entry: Option<(String, f64)>,
    /// Buy and hold's return at each [`Self::mark_close`] from the first entry on.
    buy_hold: Vec<(i64, f64)>,
    /// The margin the open lots used, summed over the bars [`Self::mark_close`] sampled from the first
    /// entry's on, and those bars.
    margin: (f64, usize),
    account: Option<TradeClosedAccountTerms>,
    /// The most held at once, after any fill: all, long, short.
    max_held: [f64; 3],
}

impl TradeClosedLedger {
    pub fn new(options: TradeClosedLedgerOptions) -> Self {
        Self { options, ..Self::default() }
    }

    /// **A ledger RESUMED from what one held**: its closed `rows` and its open `lots`, oldest first, under the
    /// default options. Every fill [`Self::apply`] books from here makes the rows and lots the ledger it was
    /// taken from would have made, because those two are all `apply` reads; what else that ledger kept serves
    /// the marked statistics only ([`Self::metrics`]), so a resumed ledger's excursions start at its rows' own
    /// entry and exit prices and its account-wide figures count from the first row it holds.
    pub fn resumed(rows: Vec<TradeClosedRow>, lots: Vec<OpenLot>) -> Self {
        let row_met = rows
            .iter()
            .map(|r| RowMet {
                met: (r.entry_price.min(r.exit_price), r.entry_price.max(r.exit_price)),
                entry_fee: 0.0,
                value: ContractValue::default(),
            })
            .collect();
        let cum = py_sum(rows.iter().map(|r| r.net));
        let mut ledger = Self { rows, lots, row_met, cum, ..Self::default() };
        ledger.note_held();
        ledger
    }

    /// The account the run started with, which the account-wide figures ([`TradeClosedAccountStatistics`]) are
    /// measured against; a ledger without one reports those figures as zero.
    #[must_use]
    pub fn with_account(mut self, account: TradeClosedAccountTerms) -> Self {
        self.account = Some(account);
        self
    }

    /// A bar of `symbol` is done (its fills booked): `walk` is the prices it went through in order
    /// (`vike_marketdata::Bar::path_points`, or a finer path), `bar` the caller's index of it, as
    /// [`Self::apply`] takes. Every lot of the symbol still held meets the walk (its entry bar from
    /// its entry price on); every row this bar closed meets it up to its exit price.
    pub fn mark_bar(&mut self, symbol: &str, bar: usize, walk: &[f64]) {
        if walk.is_empty() {
            return;
        }
        for lot in self.lots.iter_mut().filter(|l| l.symbol == symbol) {
            lot.bar_met = walk_range(walk, (lot.entry_bar == bar).then_some(lot.price), None);
            widen(&mut lot.met, lot.bar_met);
        }
        let (rows, row_met) = (&self.rows, &mut self.row_met);
        self.pending.retain(|&i| {
            let row = &rows[i];
            if row.symbol != symbol || row.exit_bar > bar {
                return true;
            }
            if row.exit_bar == bar {
                let from = (row.entry_bar == bar).then_some(row.entry_price);
                widen(&mut row_met[i].met, walk_range(walk, from, Some(row.exit_price)));
            }
            false
        });
        match self.marked.iter_mut().find(|(s, _)| s == symbol) {
            Some(m) => m.1 = bar,
            None => self.marked.push((symbol.to_string(), bar)),
        }
    }

    /// A bar's close at `ts`, every symbol's bar marked: one sample of the equity, the open lots at
    /// `mark`'s price and contract value, each less the entry commission it carries, and the intrabar
    /// drawdown and run-up, from the closed-trade equity's peak and trough before this bar to the
    /// extremes the open lots met in it. A lot `mark` cannot price counts nothing.
    pub fn mark_close(&mut self, ts: i64, mark: impl Fn(&str) -> Option<(f64, ContractValue)>) {
        let at = |pick: fn(&OpenLot) -> f64| self.cum + self.open_equity(&mark, pick);
        let close = at(|_| f64::NAN);
        let worst = at(|l| if l.side > 0 { l.bar_met.0 } else { l.bar_met.1 });
        let best = at(|l| if l.side > 0 { l.bar_met.1 } else { l.bar_met.0 });
        let i = &mut self.intrabar;
        if i.peak - worst > i.drawdown {
            (i.drawdown, i.drawdown_from) = (i.peak - worst, i.peak);
        }
        if best - i.trough > i.runup {
            (i.runup, i.runup_from) = (best - i.trough, i.trough);
        }
        (i.peak, i.trough) = (i.peak.max(self.cum), i.trough.min(self.cum));
        self.closes.push((ts, close));
        if let Some(r) = self.buy_hold_at(&mark) {
            self.buy_hold.push((ts, r));
        }
        if self.first_entry.is_some() {
            let (long, short) =
                self.account.map_or((0.0, 0.0), |a| (a.margin_long, a.margin_short));
            let used = py_sum(self.lots.iter().map(|lot| {
                let rate = if lot.side > 0 { long } else { short };
                mark(&lot.symbol).map_or(0.0, |(px, value)| {
                    value.in_settle(value.gross_notional(lot.qty, px), px) * rate
                })
            }));
            self.margin = (self.margin.0 + used, self.margin.1 + 1);
        }
    }

    /// The open lots' P&L, each less the entry commission it still carries, at `mark`'s value and at
    /// the price `price` picks from a lot (`mark`'s own price where it picks NaN).
    fn open_equity(
        &self,
        mark: &impl Fn(&str) -> Option<(f64, ContractValue)>,
        price: impl Fn(&OpenLot) -> f64,
    ) -> f64 {
        py_sum(self.lots.iter().map(|lot| {
            mark(&lot.symbol).map_or(0.0, |(last, value)| {
                let px = Some(price(lot)).filter(|p| !p.is_nan()).unwrap_or(last);
                value.in_settle(
                    value.unrealized_pnl(lot.price, px, f64::from(lot.side) * lot.qty),
                    px,
                ) - lot.entry_fee_left()
            })
        }))
    }

    /// Book one fill, and return the rows it closed (empty for an entry).
    ///
    /// `value` is what one unit of the contract is worth in the account's money
    /// (`ContractValue::in_account` already applied); `fee_rate` is what one unit of the fill's
    /// `fee` is worth in it (`1.0` when the fee is in the account's money); `bar` is the caller's
    /// bar index of the fill; `tag` names the lot an entry opens. A fill of no quantity books
    /// nothing.
    pub fn apply(
        &mut self,
        fill: &Fill,
        tag: &str,
        bar: usize,
        value: ContractValue,
        fee_rate: f64,
    ) -> &[TradeClosedRow] {
        let first = self.rows.len();
        if fill.size.is_nan() || fill.size <= 0.0 {
            return &self.rows[first..];
        }
        let fee_unit = fill.fee / fill.size * fee_rate;
        // First in, first out within the fill's symbol (`vike_model::close_lots_fifo`): each close is
        // one row, booked before its quantity comes off the lot.
        let on_close = |LotClose { lot, qty }: LotClose<'_, OpenLot>| {
            let entry_fee = if self.options.whole_entry_fee {
                lot.fee_unit * lot.qty
            } else {
                lot.fee_unit * qty
            };
            let exit_fee = fee_unit * qty;
            let size = f64::from(lot.side) * qty;
            let pnl =
                value.in_settle(value.unrealized_pnl(lot.price, fill.price, size), fill.price);
            let net = pnl - entry_fee - exit_fee;
            if self.options.whole_entry_fee {
                lot.fee_unit = 0.0;
            }
            let mut met = lot.met;
            widen(&mut met, (fill.price, fill.price));
            self.row_met.push(RowMet { met, entry_fee, value });
            if !self.marked.iter().any(|(s, b)| *s == fill.symbol && *b >= bar) {
                self.pending.push(self.rows.len());
            }
            self.cum += net;
            match self.trade_closes.last_mut() {
                Some(c) if self.rows.len() > first => {
                    (c.low, c.end) = (c.low.min(self.cum), self.cum)
                }
                _ => self.trade_closes.push(account::Close {
                    timestamp: fill.timestamp,
                    low: self.cum,
                    end: self.cum,
                }),
            }
            self.rows.push(TradeClosedRow {
                symbol: lot.symbol.clone(),
                tag: lot.tag.clone(),
                side: lot.side,
                entry_timestamp: lot.entry_timestamp,
                entry_bar: lot.entry_bar,
                entry_price: lot.price,
                exit_timestamp: fill.timestamp,
                exit_bar: bar,
                exit_price: fill.price,
                qty,
                pnl,
                commission: entry_fee + exit_fee,
                net,
            });
        };
        let same_symbol = |l: &OpenLot| l.symbol == fill.symbol;
        let left = close_lots_fifo(&mut self.lots, fill.side, fill.size, same_symbol, on_close);
        if left > LOT_DUST {
            self.first_entry.get_or_insert_with(|| (fill.symbol.clone(), fill.price));
            self.lots.push(OpenLot {
                symbol: fill.symbol.clone(),
                tag: tag.to_string(),
                side: fill.side,
                entry_timestamp: fill.timestamp,
                entry_bar: bar,
                price: fill.price,
                qty: left,
                fee_unit,
                met: (fill.price, fill.price),
                bar_met: (fill.price, fill.price),
                value,
            });
        }
        self.note_held();
        &self.rows[first..]
    }

    fn note_held(&mut self) {
        let held = |side: i32| py_sum(self.lots.iter().filter(|l| l.side == side).map(|l| l.qty));
        let (long, short) = (held(1), held(-1));
        for (max, now) in self.max_held.iter_mut().zip([long + short, long, short]) {
            *max = max.max(now);
        }
    }

    /// Every closed row, in the order the fills closed them.
    pub fn rows(&self) -> &[TradeClosedRow] {
        &self.rows
    }

    /// The lots still open, oldest first.
    pub fn lots(&self) -> &[OpenLot] {
        &self.lots
    }

    /// The trades-analysis statistics. `mark` answers a symbol's price and contract value (in the
    /// account's money) for the open lots' P&L; a lot whose symbol it cannot mark counts as open
    /// with no P&L.
    pub fn metrics(
        &self,
        mark: impl Fn(&str) -> Option<(f64, ContractValue)>,
    ) -> TradeClosedMetrics {
        let open_trades: Vec<LotOpenEntry> = self
            .lots
            .iter()
            .map(|lot| {
                let (price, pnl) = mark(&lot.symbol).map_or((None, 0.0), |(px, value)| {
                    let size = f64::from(lot.side) * lot.qty;
                    (Some(px), value.in_settle(value.unrealized_pnl(lot.price, px, size), px))
                });
                LotOpenEntry {
                    symbol: lot.symbol.clone(),
                    tag: lot.tag.clone(),
                    side: lot.side,
                    entry_timestamp: lot.entry_timestamp,
                    entry_bar: lot.entry_bar,
                    entry_price: lot.price,
                    qty: lot.qty,
                    entry_fee: lot.entry_fee_left(),
                    price,
                    pnl,
                }
            })
            .collect();
        let open: Vec<(i32, f64)> = open_trades.iter().map(|o| (o.side, o.pnl)).collect();
        let capital = self.account.map_or(0.0, |a| a.initial_capital);
        let column = |side: Option<i32>, max_held: f64| {
            let keep = |s: i32| side.is_none_or(|want| want == s);
            let (rows, percents): (Vec<&TradeClosedRow>, Vec<f64>) = self
                .rows
                .iter()
                .zip(&self.row_met)
                .filter(|(r, _)| keep(r.side))
                .map(|(r, m)| (r, row_percent(r, m)))
                .unzip();
            let open: Vec<f64> = open.iter().filter(|(s, _)| keep(*s)).map(|(_, p)| *p).collect();
            TradeClosedStatistics::of(&rows, &percents, &open, max_held, capital)
        };
        let all = column(None, self.max_held[0]);
        let trades = self.trade_list();
        let account = self.account.map(|terms| self.account_stats(terms, &all, &mark));
        let stride = self.buy_hold.len().div_ceil(BUY_HOLD_POINTS).max(1);
        let last = self.buy_hold.len().saturating_sub(1);
        let buy_hold_curve = match self.account {
            Some(a) => self
                .buy_hold
                .iter()
                .enumerate()
                .filter(|(k, _)| k % stride == 0 || *k == last)
                .map(|(_, &(t, r))| (t, a.initial_capital * (1.0 + r)))
                .collect(),
            None => Vec::new(),
        };
        TradeClosedMetrics {
            all,
            long: column(Some(1), self.max_held[1]),
            short: column(Some(-1), self.max_held[2]),
            account: account.unwrap_or_default(),
            trades,
            open_trades,
            buy_hold_curve,
        }
    }

    /// Each closed row as the reference's list of trades prints it.
    fn trade_list(&self) -> Vec<TradeClosedEntry> {
        let mut cum = 0.0;
        self.rows
            .iter()
            .zip(&self.row_met)
            .map(|(r, m)| {
                cum += r.net;
                let (runup, drawdown) =
                    excursions(r.side, r.entry_price, r.qty, m.met, m.entry_fee, m.value);
                TradeClosedEntry {
                    symbol: r.symbol.clone(),
                    tag: r.tag.clone(),
                    side: r.side,
                    entry_timestamp: r.entry_timestamp,
                    exit_timestamp: r.exit_timestamp,
                    entry_price: r.entry_price,
                    exit_price: r.exit_price,
                    qty: r.qty,
                    net: r.net,
                    net_percent: row_percent(r, m),
                    runup,
                    drawdown,
                    cum_net: cum,
                    bars: r.bars(),
                }
            })
            .collect()
    }

    /// The account-wide figures against `terms`.
    fn account_stats(
        &self,
        terms: TradeClosedAccountTerms,
        all: &TradeClosedStatistics,
        mark: &impl Fn(&str) -> Option<(f64, ContractValue)>,
    ) -> TradeClosedAccountStatistics {
        let capital = terms.initial_capital;
        let total_pnl = all.net_profit + self.open_equity(mark, |_| f64::NAN);
        let total_return = account::share(total_pnl, capital);
        let net_profit_pct = account::share(all.net_profit, capital);
        let i = self.intrabar;
        // The closed-trade equity after each row, from the first.
        let after_rows = self.rows.iter().scan(capital, |c, r| {
            *c += r.net;
            Some(*c)
        });
        let (drawdown_close_value, drawdown_close_from) = account::deepest_fall(after_rows);
        let trade_closes: Vec<account::Close> = self
            .trade_closes
            .iter()
            .map(|c| account::Close {
                timestamp: c.timestamp,
                low: capital + c.low,
                end: capital + c.end,
            })
            .collect();
        let first_row = self.rows.first().map(|r| (r.exit_timestamp, capital + r.net));
        let phases =
            first_row.map(|start| account::phases(start, &trade_closes)).unwrap_or_default();
        let [avg_dd_pct, avg_dd_days, avg_runup_pct, avg_runup_days] =
            account::phase_means(&phases);
        let top_rise = account::highest_rise(&phases);
        let points: Vec<(i64, f64)> = self.closes.iter().map(|&(t, e)| (t, capital + e)).collect();
        let (sharpe, sortino) = account::sharpe_sortino(
            &account::monthly_returns(capital, &points),
            terms.risk_free_rate,
            terms.reading,
        );
        let span_days = match (self.closes.first(), self.closes.last()) {
            (Some(a), Some(b)) => (b.0 - a.0) as f64 / 86_400_000.0,
            _ => 0.0,
        };
        let buy_hold_pct = self.buy_hold_at(mark).unwrap_or(0.0);
        // Where the equity's line ends: the last closed row's exit when nothing is open.
        let buy_hold_chart = match (self.lots.is_empty(), self.rows.last(), &self.first_entry) {
            (true, Some(r), Some((symbol, entry))) => match mark(symbol) {
                Some((_, value)) if r.symbol == *symbol => {
                    buy_hold_return(*entry, r.exit_price, value)
                }
                _ => self
                    .buy_hold
                    .iter()
                    .rev()
                    .find(|(t, _)| *t <= r.exit_timestamp)
                    .map_or(0.0, |s| s.1),
            },
            _ => buy_hold_pct,
        };
        let avg_margin = account::share(self.margin.0, self.margin.1 as f64);
        TradeClosedAccountStatistics {
            total_pnl,
            total_return,
            net_profit_percent: net_profit_pct,
            drawdown: i.drawdown,
            drawdown_percent: account::share(i.drawdown, capital + i.drawdown_from),
            drawdown_init_percent: account::share(i.drawdown, capital),
            runup: i.runup,
            runup_percent: account::share(i.runup, capital + i.runup_from),
            runup_init_percent: account::share(i.runup, capital),
            drawdown_close: drawdown_close_value,
            drawdown_close_percent: account::share(drawdown_close_value, drawdown_close_from),
            runup_close: top_rise.map_or(0.0, |r| r.money),
            runup_close_percent: top_rise.map_or(0.0, |r| r.percent),
            average_drawdown_percent: avg_dd_pct,
            average_drawdown_days: avg_dd_days,
            average_runup_percent: avg_runup_pct,
            average_runup_days: avg_runup_days,
            runup_now_percent: phases.now_rise,
            sharpe,
            sortino,
            cagr: account::cagr(net_profit_pct, span_days),
            buy_hold: capital * buy_hold_pct,
            buy_hold_percent: buy_hold_pct,
            outperformance: total_return - buy_hold_pct,
            buy_hold_chart: capital * buy_hold_chart,
            margin_calls: 0,
            average_margin: avg_margin,
            margin_eff: account::share(all.net_profit, avg_margin),
        }
    }

    /// Buy and hold's return at `mark`: one unit of the first entry's symbol from that entry's price,
    /// over what the unit cost; `None` before an entry or where `mark` cannot price it.
    fn buy_hold_at(&self, mark: &impl Fn(&str) -> Option<(f64, ContractValue)>) -> Option<f64> {
        let (symbol, entry) = self.first_entry.as_ref()?;
        let (last, value) = mark(symbol)?;
        Some(buy_hold_return(*entry, last, value))
    }

    /// The equity less the initial capital at each [`Self::mark_close`], in order.
    pub fn closes(&self) -> &[(i64, f64)] {
        &self.closes
    }
}

/// One unit's return from `entry` to `last`: its P&L over what it cost, through the contract's value.
fn buy_hold_return(entry: f64, last: f64, value: ContractValue) -> f64 {
    let pnl = value.in_settle(value.unrealized_pnl(entry, last, 1.0), last);
    account::share(pnl, value.in_settle(value.gross_notional(1.0, entry), entry))
}

/// A row's net over its entry's value (price times quantity, in the account's money), a fraction.
fn row_percent(r: &TradeClosedRow, m: &RowMet) -> f64 {
    account::share(
        r.net,
        m.value.in_settle(m.value.gross_notional(r.qty, r.entry_price), r.entry_price),
    )
}

/// A trade's favourable and adverse excursions in the account's money: its price P&L at the best and
/// the worst price it met, each less `entry_fee` (the entry commission it carries).
fn excursions(
    side: i32,
    entry: f64,
    qty: f64,
    (lo, hi): (f64, f64),
    entry_fee: f64,
    value: ContractValue,
) -> (f64, f64) {
    let size = f64::from(side) * qty;
    let at = |px: f64| value.in_settle(value.unrealized_pnl(entry, px, size), px) - entry_fee;
    let (best, worst) = if side > 0 { (hi, lo) } else { (lo, hi) };
    (at(best), at(worst))
}

/// One closed trade as the reference's list of trades prints it. Money is the account's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeClosedEntry {
    pub symbol: String,
    /// The entry's name (the list's signal), empty when the caller gave none.
    pub tag: String,
    /// `+1` long, `-1` short.
    pub side: i32,
    pub entry_timestamp: i64,
    pub exit_timestamp: i64,
    pub entry_price: f64,
    pub exit_price: f64,
    pub qty: f64,
    /// After both commission shares.
    pub net: f64,
    /// `net` over the entry's value (price times quantity), a fraction.
    pub net_percent: f64,
    /// The favourable excursion: the price P&L at the best price met, less the entry commission.
    pub runup: f64,
    /// The adverse excursion, likewise at the worst price met (negative or zero).
    pub drawdown: f64,
    /// The closed rows' net to this one, in closing order.
    pub cum_net: f64,
    pub bars: usize,
}

/// One trade still open when the run ended: a lot first-in first-out has not closed, at the run's
/// last mark. It is no closed row, so no statistic counts it; it is listed so a reader that books an
/// open trade its own way (a reference list that closes it at the last bar, with an exit commission)
/// has every fact of the entry. Money is the account's.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LotOpenEntry {
    pub symbol: String,
    /// The entry's name (the list's signal), empty when the caller gave none.
    pub tag: String,
    /// `+1` long, `-1` short.
    pub side: i32,
    pub entry_timestamp: i64,
    /// The caller's bar index of the entry fill (a run's: its bar series).
    pub entry_bar: usize,
    pub entry_price: f64,
    /// What is still open of the lot.
    pub qty: f64,
    /// The entry commission it still carries (all of its share, or none once
    /// [`TradeClosedLedgerOptions::whole_entry_fee`] booked it on an earlier row).
    pub entry_fee: f64,
    /// The mark it was valued at, `None` where the run had no price for it.
    pub price: Option<f64>,
    /// The price P&L at that mark, before any commission (zero with no mark): its share of
    /// [`TradeClosedStatistics::open_pnl`].
    pub pnl: f64,
}

/// One column of the trades analysis. Every money figure is NET of commission (the row's `net`),
/// every count is of closed rows; a ratio with nothing under it is `0.0`, except
/// [`Self::profit_factor`]'s house `inf` when nothing lost and something won.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeClosedStatistics {
    pub trades: usize,
    pub winners: usize,
    pub losers: usize,
    /// Rows whose net is exactly zero.
    pub even: usize,
    /// `winners / trades`, the even rows in the denominator.
    pub percent_profitable: f64,
    pub net_profit: f64,
    pub gross_profit: f64,
    /// Positive: the losing rows' sum, negated.
    pub gross_loss: f64,
    #[serde(
        serialize_with = "crate::report::serialize_f64_null_when_nonfinite",
        deserialize_with = "crate::report::de_f64_null_as_infinity"
    )]
    pub profit_factor: f64,
    /// `net_profit / trades`.
    pub expected_payoff: f64,
    pub average_win: f64,
    /// Negative, as the losing rows are.
    pub average_loss: f64,
    /// `average_win / |average_loss|`.
    pub win_loss_ratio: f64,
    pub largest_win: f64,
    /// Negative.
    pub largest_loss: f64,
    /// `largest_win / gross_profit`, a fraction.
    pub largest_win_percent: f64,
    /// `|largest_loss| / gross_loss`, a fraction.
    pub largest_loss_percent: f64,
    pub average_bars: f64,
    pub average_bars_win: f64,
    pub average_bars_loss: f64,
    /// The most held at once after any fill (all lots, or the side's).
    pub max_held: f64,
    /// The closed rows' commission, entry and exit shares.
    pub commission: f64,
    pub open_trades: usize,
    /// The open lots' price P&L at the mark, before any commission.
    pub open_pnl: f64,
    /// The mean of the rows' net percents (each its net over its entry's value), a fraction: the
    /// reference's expectancy in percent (its money twin is [`Self::expected_payoff`]).
    pub expectancy_percent: f64,
    /// Rows whose net percent lies more than two deviations (`N - 1`) from the rows' mean percent.
    pub outliers: usize,
    /// Those rows' net.
    pub outliers_pnl: f64,
    /// [`Self::outliers_pnl`] over the initial capital (zero without one).
    pub outliers_pnl_percent: f64,
    /// The mean net percent of the winning rows that are not outliers, a fraction.
    pub average_win_percent: f64,
    /// The mean net percent of the losing rows that are not outliers (negative).
    pub average_loss_percent: f64,
    /// The most winning rows in a row, in closing order; a losing or even row ends a run.
    pub win_streak: usize,
    /// The most losing rows in a row, likewise.
    pub loss_streak: usize,
    /// The mean length of the winning runs.
    pub average_win_streak: f64,
    /// The mean length of the losing runs.
    pub average_loss_streak: f64,
}

impl TradeClosedStatistics {
    /// The column over `rows` (in closing order) and their net percents `percents`, beside them.
    fn of(
        rows: &[&TradeClosedRow],
        percents: &[f64],
        open: &[f64],
        max_held: f64,
        capital: f64,
    ) -> Self {
        let mean = |xs: &[f64]| {
            if xs.is_empty() { 0.0 } else { py_sum(xs.iter().copied()) / xs.len() as f64 }
        };
        let bars_where = |keep: &dyn Fn(f64) -> bool| -> Vec<f64> {
            rows.iter().filter(|r| keep(r.net)).map(|r| r.bars() as f64).collect()
        };
        let wins: Vec<f64> = rows.iter().map(|r| r.net).filter(|n| *n > 0.0).collect();
        let losses: Vec<f64> = rows.iter().map(|r| r.net).filter(|n| *n < 0.0).collect();
        let trades = rows.len();
        let net_profit = py_sum(rows.iter().map(|r| r.net));
        let gross_profit = py_sum(wins.iter().copied());
        let gross_loss = -py_sum(losses.iter().copied());
        let profit_factor = if gross_loss == 0.0 {
            if gross_profit > 0.0 { f64::INFINITY } else { 0.0 }
        } else {
            gross_profit / gross_loss
        };
        let (average_win_value, average_loss_value) = (mean(&wins), mean(&losses));
        let largest_win = wins.iter().copied().fold(0.0, f64::max);
        let largest_loss = losses.iter().copied().fold(0.0, f64::min);
        let outlier = outliers(percents);
        let kept = |want: fn(f64) -> bool| -> Vec<f64> {
            rows.iter()
                .zip(percents)
                .zip(&outlier)
                .filter(|((r, _), out)| !**out && want(r.net))
                .map(|((_, p), _)| *p)
                .collect()
        };
        let outliers_pnl =
            py_sum(rows.iter().zip(&outlier).filter(|(_, out)| **out).map(|(r, _)| r.net));
        let [win_runs, loss_runs] = [1, -1].map(|side| runs(rows, side));
        TradeClosedStatistics {
            trades,
            winners: wins.len(),
            losers: losses.len(),
            even: trades - wins.len() - losses.len(),
            percent_profitable: if trades == 0 { 0.0 } else { wins.len() as f64 / trades as f64 },
            net_profit,
            gross_profit,
            gross_loss,
            profit_factor,
            expected_payoff: if trades == 0 { 0.0 } else { net_profit / trades as f64 },
            average_win: average_win_value,
            average_loss: average_loss_value,
            win_loss_ratio: if average_loss_value == 0.0 {
                0.0
            } else {
                average_win_value / -average_loss_value
            },
            largest_win,
            largest_loss,
            largest_win_percent: if gross_profit > 0.0 { largest_win / gross_profit } else { 0.0 },
            largest_loss_percent: if gross_loss > 0.0 { -largest_loss / gross_loss } else { 0.0 },
            average_bars: mean(&bars_where(&|_| true)),
            average_bars_win: mean(&bars_where(&|n| n > 0.0)),
            average_bars_loss: mean(&bars_where(&|n| n < 0.0)),
            max_held,
            commission: py_sum(rows.iter().map(|r| r.commission)),
            open_trades: open.len(),
            open_pnl: py_sum(open.iter().copied()),
            expectancy_percent: mean(percents),
            outliers: outlier.iter().filter(|o| **o).count(),
            outliers_pnl,
            outliers_pnl_percent: account::share(outliers_pnl, capital),
            average_win_percent: mean(&kept(|n| n > 0.0)),
            average_loss_percent: mean(&kept(|n| n < 0.0)),
            win_streak: win_runs.iter().copied().max().unwrap_or(0),
            loss_streak: loss_runs.iter().copied().max().unwrap_or(0),
            average_win_streak: mean(&win_runs.iter().map(|n| *n as f64).collect::<Vec<_>>()),
            average_loss_streak: mean(&loss_runs.iter().map(|n| *n as f64).collect::<Vec<_>>()),
        }
    }

    /// The value of one field by its name (a [`crate::metric_catalog::METRICS`] id without its
    /// `closed_` prefix).
    pub(crate) fn value_of(&self, field: &str) -> Option<f64> {
        Some(match field {
            "trades" => self.trades as f64,
            "winners" => self.winners as f64,
            "losers" => self.losers as f64,
            "even" => self.even as f64,
            "percent_profitable" => self.percent_profitable,
            "net_profit" => self.net_profit,
            "gross_profit" => self.gross_profit,
            "gross_loss" => self.gross_loss,
            "profit_factor" => self.profit_factor,
            "expected_payoff" => self.expected_payoff,
            "average_win" => self.average_win,
            "average_loss" => self.average_loss,
            "win_loss_ratio" => self.win_loss_ratio,
            "largest_win" => self.largest_win,
            "largest_loss" => self.largest_loss,
            "largest_win_percent" => self.largest_win_percent,
            "largest_loss_percent" => self.largest_loss_percent,
            "average_bars" => self.average_bars,
            "average_bars_win" => self.average_bars_win,
            "average_bars_loss" => self.average_bars_loss,
            "max_held" => self.max_held,
            "commission" => self.commission,
            "open_trades" => self.open_trades as f64,
            "open_pnl" => self.open_pnl,
            "expectancy_percent" => self.expectancy_percent,
            "outliers" => self.outliers as f64,
            "outliers_pnl" => self.outliers_pnl,
            "outliers_pnl_percent" => self.outliers_pnl_percent,
            "average_win_percent" => self.average_win_percent,
            "average_loss_percent" => self.average_loss_percent,
            "win_streak" => self.win_streak as f64,
            "loss_streak" => self.loss_streak as f64,
            "average_win_streak" => self.average_win_streak,
            "average_loss_streak" => self.average_loss_streak,
            _ => return None,
        })
    }
}

/// Which of `percents` lie more than two deviations (`N - 1`) from their mean: the reference's outliers,
/// measured on its reports (the outliers' net and the averages without them, to the cent and the
/// percent's last decimal). None for fewer than two.
fn outliers(percents: &[f64]) -> Vec<bool> {
    let n = percents.len();
    if n < 2 {
        return vec![false; n];
    }
    let m = py_sum(percents.iter().copied()) / n as f64;
    let sd = (py_sum(percents.iter().map(|p| (p - m) * (p - m))) / (n - 1) as f64).sqrt();
    percents.iter().map(|p| (p - m).abs() > 2.0 * sd).collect()
}

/// The lengths of the runs of rows on `side` (`1` winning, `-1` losing), in order; an even row, or
/// one on the other side, ends a run.
fn runs(rows: &[&TradeClosedRow], side: i32) -> Vec<usize> {
    let mut out = Vec::new();
    let mut run = 0;
    for r in rows {
        if f64::from(side) * r.net > 0.0 {
            run += 1;
        } else if run > 0 {
            out.push(run);
            run = 0;
        }
    }
    if run > 0 {
        out.push(run);
    }
    out
}

/// The closed-trade block of a report: the statistics in the reference's three columns, the
/// account-wide figures and the list of trades.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TradeClosedMetrics {
    pub all: TradeClosedStatistics,
    pub long: TradeClosedStatistics,
    pub short: TradeClosedStatistics,
    /// Zero where the ledger had no [`TradeClosedAccountTerms`].
    pub account: TradeClosedAccountStatistics,
    /// Every closed row, in closing order.
    pub trades: Vec<TradeClosedEntry>,
    /// The trades still open at the end, oldest entry first ([`TradeClosedStatistics::open_trades`]
    /// counts them). Empty when the run ended flat, and in a block written before 2026-11-10.
    pub open_trades: Vec<LotOpenEntry>,
    /// Buy and hold's equity (the initial capital held from the first entry) at bar closes, at most
    /// [`BUY_HOLD_POINTS`] of them evenly and the last: the line a report draws beside its equity.
    /// Empty where the ledger had no [`TradeClosedAccountTerms`].
    pub buy_hold_curve: Vec<(i64, f64)>,
}

impl TradeClosedMetrics {
    /// The value of one [`crate::metric_catalog::MetricHome::ClosedTrades`] id: the field after the
    /// `closed_` prefix, of the ALL column or else of the account figures. `None` for any other id.
    pub fn value_of(&self, id: &str) -> Option<f64> {
        let field = id.strip_prefix("closed_")?;
        self.all.value_of(field).or_else(|| self.account.value_of(field))
    }
}

#[path = "closed_trades_tests.rs"]
#[cfg(test)]
mod closed_trades_tests;
