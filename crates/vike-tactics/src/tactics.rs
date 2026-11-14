//! [`Tactics`], what a rule sees on a bar (by convention its parameter is named `strategy`): reads of
//! the account and of the language's own book, and the order verbs. A verb returns a builder that
//! records the order; the order is LOWERED when the next verb starts or the hook returns, in call
//! order, onto `vike_model::OrderBroker` alone:
//!
//! - an entry is a tagged market, stop, stop-limit or limit order, or with `.chase()` a pegged one
//!   and with `.iceberg(visible)` a limit showing `visible` of its size, sized by the book's rules
//!   (an opposite position adds itself, the pyramiding limit refuses, a size of 0 only closes);
//! - a close is a reduce-only market order (`OrderBroker::place_at_close` with `.here()`);
//! - an exit's stop, target and trailing leg are reduce-only legs of one exit
//!   (`OrderBroker::place_exit_leg`), which the language links itself: a leg's fill cancels the
//!   others by tag, and a fired exit stays spent while its lots live;
//! - naming an order again AMENDS it in place when the seam can (same kind, side and flag; never a
//!   pegged order or an iceberg), else cancels it and places a new one. Every order gets its own tag
//!   (`<name>#<n>`), so a tag never names two orders;
//! - terms no kind states (`.chase()` with a level, `.iceberg(visible)` without a limit or not above
//!   zero) are refused by name: `OrderBroker::deny` ends the order `Denied` in the language's words,
//!   never dropped.
//!
//! Reads change only on fills, so a read between two verbs is the same whether the first was sent.
//!
//! **Several symbols.** Every verb and read acts on the CURRENT symbol, which each hook starts on
//! the run's own. [`Tactics::on`] makes another current (the run's book of it, `crate::RuleRun::trades`)
//! until the next `on` or the hook's end; [`Tactics::position_of`] and [`Tactics::price_of`] read
//! any symbol without moving. An order on the run's own symbol is tagged `<name>#<n>`, on another
//! `<symbol>/<name>#<n>`.

use vike_model::{
    ContingencyType, LOT_DUST, OrderBroker, OrderListLeg, OrderPeg, OrderPegAnchor, OrderPlacement,
    OrderType, TimeInForce, TradeClosedRow, closing_side, snap_limit, snap_stop, stop_loss_price,
    take_profit_price, trailing_activation_reached,
};

use crate::book::{
    Books, TacticsBookDormantExit, TacticsBookLeg, TacticsBookSentOrder, TacticsBookTicket,
    TacticsBookWaitingGuard, TacticsBookWaitingTrail, activation, touch_level, touch_ticket,
};
use crate::header::RuleDefaults;
use crate::input::TacticsInput;
use crate::lots::{TacticsBookBinding, TacticsBookLot};
use crate::sizing::TacticsSizing;
use crate::verbs::{Level, TacticsClose, TacticsEntry, TacticsExit, TacticsOrder, TacticsTrail};
use vike_model::OrderSide;

/// What a run states once, read by every hook.
pub(crate) struct Setup {
    pub(crate) header: RuleDefaults,
    pub(crate) stacked_closes: bool,
    pub(crate) fills_a_day: Option<usize>,
    pub(crate) touch_at_market: bool,
    /// What a market or stop fill on the own symbol is expected to pay over its price
    /// (`crate::RuleRun::slippage`).
    pub(crate) slippage: f64,
    /// The quantity step a size on the own symbol is floored to (`crate::RuleRun::qty_step`).
    pub(crate) qty_step: Option<f64>,
    /// The tick of the own symbol (`crate::RuleRun::tick`).
    pub(crate) tick: Option<f64>,
    /// The bars the verbs act on (`crate::RuleRun::trading`), `None` for every bar.
    pub(crate) window: Option<(i64, i64)>,
}

impl Setup {
    /// The tick a level on `symbol` snaps to and a trail counts in: the run's on its `own` symbol
    /// (`crate::RuleRun::tick`), else the broker's grid for it.
    pub(crate) fn tick_of(&self, broker: &dyn OrderBroker, symbol: &str, own: bool) -> Option<f64> {
        let run = self.tick.filter(|_| own);
        run.or_else(|| broker.instrument_grid(symbol)?.tick_size)
    }
}

/// The order a builder is recording, at most one at a time.
#[derive(Default)]
struct Pending {
    entry: Option<TacticsEntry>,
    order: Option<TacticsOrder>,
    close: Option<TacticsClose>,
    exit: Option<TacticsExit>,
}

/// What a rule sees on a bar: reads of the account, and the order verbs (the module doc).
pub struct Tactics<'a> {
    broker: &'a mut dyn OrderBroker,
    /// Every symbol's book; as a `Book` it is the current symbol's.
    book: &'a mut Books,
    setup: &'a Setup,
    /// The current symbol's tick (`Setup::tick_of`).
    tick: Option<f64>,
    /// The pass is inside the run's window; `live` adds the current symbol's fills-a-day cap at `ts`.
    open: bool,
    timestamp: i64,
    live: bool,
    /// The broker refuses a reduce-only order while flat (`OrderBroker::rests_reduce_only_while_flat`
    /// is `false`, every live broker): an exit issued with its entry goes as that entry's OTO child.
    oto: bool,
    pending: Pending,
}

impl<'a> Tactics<'a> {
    /// A pass at `ts` starting on the book `at` (`open`: inside the run's window).
    pub(crate) fn new(
        broker: &'a mut dyn OrderBroker,
        book: &'a mut Books,
        setup: &'a Setup,
        at: usize,
        open: bool,
        ts: i64,
    ) -> Self {
        let oto = !broker.rests_reduce_only_while_flat();
        let pending = Pending::default();
        let mut strategy = Tactics {
            broker,
            book,
            setup,
            tick: None,
            open,
            timestamp: ts,
            live: false,
            oto,
            pending,
        };
        strategy.switch(at);
        strategy
    }

    /// Makes the book `at` current, with its symbol's tick and fills-a-day cap.
    fn switch(&mut self, at: usize) {
        self.book.at = at;
        self.tick = self.setup.tick_of(&*self.broker, &self.book.symbol, at == 0);
        self.live = self.open && self.book.uncapped(self.timestamp);
    }
}

/// `ticket` as a leg of an order list: an OTO child of the leg `parent`, cancelling `linked` (OCO).
/// Never a trailing order today: the language sends a trailing leg alone, priced from its entry's fill,
/// and links it to its exit's other legs itself.
fn list_leg(ticket: &TacticsBookTicket, parent: Option<usize>, linked: Vec<usize>) -> OrderListLeg {
    let (order_type, price, trigger_price) = match ticket.kind {
        OrderPlacement::Market => (OrderType::Market, None, None),
        OrderPlacement::Limit { price } => (OrderType::Limit, Some(price), None),
        OrderPlacement::Stop { trigger } => (OrderType::Stop, None, Some(trigger)),
        OrderPlacement::StopLimit { trigger, limit } => {
            (OrderType::StopLimit, Some(limit), Some(trigger))
        }
        OrderPlacement::TakeProfit { trigger, limit } => {
            (OrderType::TakeProfit, limit, Some(trigger))
        }
        OrderPlacement::Trailing { activation, .. } => (OrderType::Trailing, None, activation),
        OrderPlacement::Pegged { .. } | OrderPlacement::Iceberg { .. } => {
            unreachable!("a standalone order is never held for a list (`Tactics::send_entry`)")
        }
    };
    OrderListLeg {
        side: ticket.side,
        qty: ticket.size,
        order_type,
        price,
        trigger_price,
        trail: match ticket.kind {
            OrderPlacement::Trailing { trail, .. } => Some(trail),
            _ => None,
        },
        reduce_only: ticket.reduce_only,
        time_in_force: TimeInForce::Gtc,
        parent,
        contingency: (!linked.is_empty()).then_some(ContingencyType::Oco),
        linked,
        order_client_id: None,
    }
}

impl Tactics<'_> {
    /// The symbol the verbs and reads act on: the run's own, or the one [`Tactics::on`] made current.
    pub fn symbol(&self) -> &str {
        &self.book.symbol
    }

    /// Trades `symbol` from here on: the order a builder is recording is lowered first, and every
    /// verb and read after it (`position`, `price`, `sizing`, `tick`, `lots`, ...) is `symbol`'s
    /// until the next `on` or the hook's end (each hook starts on the run's own). `""` and the run's
    /// own symbol name its own. One the run did not declare (`crate::RuleRun::trades`) gets its book on
    /// first use, and the broker answers for it as for any symbol: the simulator refuses an order
    /// on a symbol its run does not hold, by name. ⚠ Live, a mount trades the symbols its run
    /// declared (`crate::RuleRun::trades`, which the daemon declares as the mount's legs) and REFUSES
    /// every order on another symbol.
    pub fn on(&mut self, symbol: &str) -> &mut Self {
        self.flush();
        let at = self.book.add(symbol);
        if at != self.book.at {
            self.switch(at);
        }
        self
    }

    /// The position on `symbol`, whichever symbol is current: positive long, negative short.
    pub fn position_of(&self, symbol: &str) -> f64 {
        self.broker.position(symbol)
    }

    /// The last mark of `symbol`, whichever symbol is current (`vike_model::OrderBroker::price`).
    pub fn price_of(&self, symbol: &str) -> f64 {
        self.broker.price(symbol)
    }

    /// The position: positive long, negative short.
    pub fn position(&self) -> f64 {
        self.broker.position(&self.book.symbol)
    }

    /// The average entry price of the open lots, first in first out, NaN when none is open: one lot's
    /// own price, several averaged by the contract's value (`vike_model::ContractValue::add_quantity_price`,
    /// the symbol's `vike_model::OrderBroker::contract_value`). Not the engine's position average
    /// (`vike_model::OrderBroker::average_price`), which keeps its price through a partial close where
    /// first in, first out drops the oldest lot's.
    pub fn avg_cost(&self) -> f64 {
        match self.book.lots.as_slice() {
            [] => f64::NAN,
            [one] => one.price,
            many => {
                let value = self.broker.contract_value(&self.book.symbol).unwrap_or_default();
                let (held, quantity_price) =
                    many.iter().fold((0.0, 0.0), |(held, quantity_price), l| {
                        (
                            held + l.qty,
                            value.add_quantity_price(held, quantity_price, l.qty, l.price),
                        )
                    });
                quantity_price / held
            }
        }
    }

    /// Account equity now (`vike_model::OrderBroker::equity`).
    pub fn equity(&self) -> f64 {
        self.broker.equity()
    }

    /// What a size is worked out from, now: the equity, the symbol's value
    /// (`vike_model::OrderBroker::contract_value`, linear 1 when the broker states none), the header's
    /// commission, and the run's slippage and quantity step (else the lot size of the broker's grid);
    /// on a symbol not the run's own, no slippage and the grid's lot size.
    pub fn sizing(&self) -> TacticsSizing {
        let (symbol, own) = (&self.book.symbol, self.book.at == 0);
        let grid_step = || self.broker.instrument_grid(symbol).and_then(|g| g.lot_size);
        TacticsSizing {
            equity: self.broker.equity(),
            value: self.broker.contract_value(symbol).unwrap_or_default(),
            commission: self.setup.header.fee,
            slippage: if own { self.setup.slippage } else { 0.0 },
            qty_step: self.setup.qty_step.filter(|_| own).or_else(grid_step),
        }
    }

    /// The last mark of the symbol (`vike_model::OrderBroker::price`).
    pub fn price(&self) -> f64 {
        self.broker.price(&self.book.symbol)
    }

    /// The run's trading window as `crate::RuleRun::trading` states it, `(from_ms, to_ms)` with `to_ms` the
    /// window's last bar; `None` when the run states none (a paper or live mount, which has no end).
    /// What a rule that reads its run's clock reads, never a date of its own.
    pub fn window(&self) -> Option<(i64, i64)> {
        self.setup.window
    }

    /// The input `name` as of now ([`crate::TacticsInput`]): its bars that have closed, oldest first, what the
    /// broker answers (`vike_model::OrderBroker::input_bars`). `name` is one the rule declares
    /// ([`crate::Rule::inputs`]); any other reads nothing, as an input before its first bar closes does.
    pub fn input(&self, name: &str) -> TacticsInput<'_> {
        TacticsInput::new(self.broker.input_bars(name))
    }

    /// The run's closed trades so far, in the order the fills closed them: the broker's closed-trade
    /// ledger (`vike_model::OrderBroker::closed_trades`, one row per part of an entry a fill closed, each
    /// net of its share of both commissions), the one the run's report counts, never a second booking.
    /// In a backtest of one strategy they are the rule's own. A fill's rows are in it before the rule
    /// hears that fill, so an [`crate::Rule::after_fill`] reads the row its fill closed.
    ///
    /// `None` where the broker keeps no ledger, which is NOT "nothing closed yet": the simulator and a
    /// paper or live mount always keep one, empty before the first close.
    pub fn closed_trades(&self) -> Option<&[TradeClosedRow]> {
        self.broker.closed_trades()
    }

    /// The net of [`Tactics::closed_trades`], each row's added in closing order from `0.0`; `None`
    /// where the broker keeps no ledger.
    pub fn closed_net(&self) -> Option<f64> {
        self.closed_trades().map(|rows| rows.iter().fold(0.0, |net, row| net + row.net))
    }

    /// The lots still open, oldest first, each named by the entry it is.
    pub fn lots(&self) -> &[TacticsBookLot] {
        &self.book.lots
    }

    /// The entries still bound, oldest first (`crate::RuleRun::bound_exits`).
    pub fn bindings(&self) -> &[TacticsBookBinding] {
        &self.book.bindings
    }

    /// Where a stop for `side` at `price` lands on the tick grid: a buy stop up, a sell stop down
    /// (`vike_model::snap_stop`). The price itself when no tick is known.
    pub fn snap_stop(&self, side: OrderSide, price: f64) -> f64 {
        snap_stop(i32::from(side), price, self.tick)
    }

    /// Where a limit for `side` at `price` lands: a buy limit down, a sell limit up
    /// (`vike_model::snap_limit`).
    pub fn snap_limit(&self, side: OrderSide, price: f64) -> f64 {
        snap_limit(i32::from(side), price, self.tick)
    }

    /// The tick the two snaps above and a trail use (`crate::RuleRun::tick`, else the broker's grid),
    /// `None` when neither is known: what a front end that snaps outside this object reads
    /// (`vike-rhai`'s `strategy.snap_stop`).
    pub fn tick(&self) -> Option<f64> {
        self.tick
    }

    /// An entry under `name`: a market order unless it states a level. Naming one again replaces
    /// it. Does nothing outside the trading window.
    pub fn entry(&mut self, name: &str, side: OrderSide) -> &mut TacticsEntry {
        self.flush();
        self.pending.entry.insert(TacticsEntry::new(name, side, self.setup.header.size))
    }

    /// A free order under `name`, neither an entry nor a close.
    pub fn order(&mut self, name: &str, side: OrderSide) -> &mut TacticsOrder {
        self.flush();
        let entry = TacticsEntry::new(name, side, self.setup.header.size);
        self.pending.order.insert(TacticsOrder { entry })
    }

    /// A close of the lots of `name` (what is bound to them under `crate::RuleRun::bound_exits`). A
    /// close of a name not held does nothing.
    pub fn close(&mut self, name: &str) -> &mut TacticsClose {
        self.flush();
        self.pending.close.insert(TacticsClose {
            lots: Some(name.to_string()),
            size: None,
            here: false,
        })
    }

    /// A close of the whole position.
    pub fn close_all(&mut self) -> &mut TacticsClose {
        self.flush();
        self.pending.close.insert(TacticsClose { lots: None, size: None, here: false })
    }

    /// An exit named `name` protecting the lots of `name` (another's with [`TacticsExit::from`]), or an
    /// entry of that name still to come. Issuing it again moves its legs in place; one that has
    /// fired stays spent while its lots live.
    pub fn exit(&mut self, name: &str) -> &mut TacticsExit {
        self.flush();
        self.pending.exit.insert(TacticsExit {
            name: name.to_string(),
            lots: name.to_string(),
            size: None,
            take: None,
            stop: None,
            trail: None,
            group: None,
        })
    }

    /// Cancels every order under `name` (on the current symbol), all legs of an exit included, and
    /// what it waits for or holds.
    pub fn cancel(&mut self, name: &str) {
        self.flush();
        self.book.waiting.retain(|w| w.name != name);
        self.book.waiting_guards.retain(|w| w.name != name);
        self.book.dormant.retain(|d| d.name != name);
        self.book.pull_where(self.broker, |n| n.name == name);
    }

    /// Cancels every order on the current symbol.
    pub fn cancel_all(&mut self) {
        self.flush();
        self.book.waiting.clear();
        self.book.waiting_guards.clear();
        self.book.dormant.clear();
        self.book.pull_where(self.broker, |_| true);
    }

    /// The end of a hook: the last builder's order, then what the pass held back on each symbol,
    /// the run's own first.
    pub(crate) fn finish(&mut self) {
        self.flush();
        for at in 0..self.book.all.len() {
            self.book.at = at;
            self.send_held();
        }
    }

    /// Lowers the order a builder recorded, if any.
    fn flush(&mut self) {
        if let Some(e) = self.pending.entry.take() {
            self.send_entry(e);
        }
        if let Some(o) = self.pending.order.take() {
            self.send_free(o.entry);
        }
        if let Some(c) = self.pending.close.take() {
            self.send_close(c);
        }
        if let Some(x) = self.pending.exit.take() {
            self.send_exit(x);
        }
    }

    fn stop_at(&self, side: i32, price: f64) -> f64 {
        snap_stop(side, price, self.tick)
    }

    fn limit_at(&self, side: i32, price: f64) -> f64 {
        snap_limit(side, price, self.tick)
    }

    /// The kind an entry or a free order states: a market, a stop, a limit, or with both a
    /// stop-limit, whose trigger snaps as a limit does; chased, a pegged order at its own side of the
    /// touch; a limit with a visible size, an iceberg. `Err`, in words naming the verb, for terms no
    /// kind states: a chase with a level or a visible size, a visible size with no limit or with a
    /// stop, or one not above zero (a hidden order).
    fn kind_of(&self, side: i32, e: &TacticsEntry) -> Result<OrderPlacement, String> {
        let kind = match (e.stop, e.limit) {
            (None, None) => OrderPlacement::Market,
            (Some(level), None) => OrderPlacement::Stop { trigger: self.stop_at(side, level) },
            (None, Some(level)) => OrderPlacement::Limit { price: self.limit_at(side, level) },
            (Some(level), Some(limit)) => OrderPlacement::StopLimit {
                trigger: self.limit_at(side, level),
                limit: self.limit_at(side, limit),
            },
        };
        let name = &e.name;
        match (e.chase, e.display, kind) {
            (false, None, kind) => Ok(kind),
            (true, None, OrderPlacement::Market) => Ok(OrderPlacement::Pegged {
                peg: OrderPeg { anchor: OrderPegAnchor::Best, offset: 0.0 },
            }),
            (true, ..) => Err(format!(
                "order \"{name}\": .chase() rests at the touch, so it states no .stop(), .limit() or \
                 .iceberg(): order refused"
            )),
            (false, Some(display), OrderPlacement::Limit { price }) if display > 0.0 => {
                Ok(OrderPlacement::Iceberg { price, display })
            }
            (false, Some(display), OrderPlacement::Limit { .. }) => Err(format!(
                "order \"{name}\": .iceberg({display}) shows no slice: the visible size must be above \
                 zero (a hidden order is refused): order refused"
            )),
            (false, Some(display), _) => Err(format!(
                "order \"{name}\": .iceberg({display}) is a limit's visible size, so it needs \
                 .limit(level) and no .stop(): order refused"
            )),
        }
    }

    /// Refuses, in `why`'s words, the order the rule stated under `name` (`leg`): nothing is sent,
    /// and the broker ends it `Denied` under a tag of its own (`OrderBroker::deny`), the path an
    /// order the broker refuses at its intake takes. As a re-issue the seam cannot amend
    /// (`Tactics::place`), it cancels the order resting under that name and leg, and says so.
    fn refuse(&mut self, name: &str, leg: TacticsBookLeg, size: f64, why: String) {
        let resting = self.book.named.iter().any(|n| n.rests() && n.name == name && n.leg == leg);
        self.book.pull_where(self.broker, |n| n.name == name && n.leg == leg);
        let why = if resting {
            format!("{why}; the order resting under \"{name}\" is cancelled")
        } else {
            why
        };
        let tag = self.book.new_tag(name);
        self.broker.deny(&self.book.symbol, &tag, size, &why);
    }

    fn send_entry(&mut self, e: TacticsEntry) {
        let side = i32::from(e.side);
        let kind = match self.kind_of(side, &e) {
            Ok(kind) => kind,
            Err(why) if self.live => return self.refuse(&e.name, TacticsBookLeg::Lot, e.size, why),
            // outside the trading window a verb does nothing
            Err(_) => return,
        };
        if let Some(full) = self.entry_size(&e.name, side, e.size) {
            let ticket = TacticsBookTicket::new(kind, side, full);
            // a standalone entry goes alone, so an exit issued with it waits for its fill
            let hold = self.oto && !ticket.standalone();
            let (name, group) = (e.name.as_str(), e.group.as_deref());
            let at = self.place(name, TacticsBookLeg::Lot, name, group, ticket, false, hold);
            self.book.named[at].sizes = Some((e.size, full));
        }
    }

    fn send_free(&mut self, e: TacticsEntry) {
        if !(self.live && e.size > 0.0) {
            return;
        }
        let side = i32::from(e.side);
        match self.kind_of(side, &e) {
            Ok(kind) => {
                let ticket = TacticsBookTicket::new(kind, side, e.size);
                self.place(
                    &e.name,
                    TacticsBookLeg::Free,
                    &e.name,
                    e.group.as_deref(),
                    ticket,
                    false,
                    false,
                );
            }
            Err(why) => self.refuse(&e.name, TacticsBookLeg::Free, e.size, why),
        }
    }

    fn send_close(&mut self, c: TacticsClose) {
        let (lots, whole) = match &c.lots {
            Some(name) => (name.as_str(), self.book.held_in(name)),
            None => ("", self.position().abs()),
        };
        let size = c.size.map_or(whole, |s| s.min(whole));
        if self.live && size > 0.0 {
            let ticket = TacticsBookTicket::exit(
                OrderPlacement::Market,
                closing_side(self.position()),
                size,
            );
            let name = if self.setup.stacked_closes {
                format!("drop {lots} {}", self.book.next + 1)
            } else {
                format!("drop {lots}")
            };
            self.place(&name, TacticsBookLeg::Drop, lots, None, ticket, c.here, false);
        }
    }

    /// The size the entry `name` takes (`Tactics::lot_size`). An entry of size 0 with nothing to
    /// close cancels the one resting under its name, and so does an entry the pyramiding limit
    /// refuses.
    fn entry_size(&mut self, name: &str, side: i32, size: f64) -> Option<f64> {
        let full = self.lot_size(side, size);
        let held = self.position();
        let at_cap = size > 0.0
            && held != 0.0
            && (held > 0.0) == (side > 0)
            && self.book.entries_held() >= self.setup.header.pyramiding;
        if full.is_none() && self.live && (size == 0.0 || at_cap) {
            self.book.pull_where(self.broker, |n| n.name == name && n.leg == TacticsBookLeg::Lot);
        }
        full
    }

    /// The size an entry takes, fixed while it rests: an opposite position adds its own size, less
    /// any market close of it already sent this pass. A market entry the other way already sent this
    /// pass fills first, so this one meets the position that leaves: that entry's asked size, on top
    /// of the position held when it was on that entry's side. `None` when the side holds its
    /// pyramiding limit. An entry of size 0 only closes: `None` when there is nothing to close.
    fn lot_size(&self, side: i32, size: f64) -> Option<f64> {
        let market = |leg: TacticsBookLeg, on: i32| {
            self.book.named.iter().filter(move |n| {
                n.rests()
                    && n.leg == leg
                    && n.ticket.side == on
                    && n.ticket.kind == OrderPlacement::Market
            })
        };
        let closes = || -> f64 { market(TacticsBookLeg::Drop, side).map(|n| n.ticket.size).sum() };
        let ahead: f64 = market(TacticsBookLeg::Lot, -side)
            .filter_map(|n| n.sizes)
            .map(|(asked, _)| asked)
            .sum();
        let held = self.position();
        let full = if !self.live || size.is_nan() || size < 0.0 {
            None
        } else if ahead > 0.0 {
            let kept =
                if held != 0.0 && (held > 0.0) == (side < 0) { held.abs() - closes() } else { 0.0 };
            Some(size + kept.max(0.0) + ahead)
        } else if held == 0.0 {
            Some(size)
        } else if (held > 0.0) == (side > 0) {
            (self.book.entries_held() < self.setup.header.pyramiding).then_some(size)
        } else {
            Some(size + (held.abs() - closes()).max(0.0))
        };
        full.filter(|&full| full > 0.0)
    }

    /// Sends an order under a name, or moves the one already resting under that name and leg in
    /// place: an amend, which keeps its place in the engine's queue. One the seam cannot amend
    /// (another kind, side or trail, or a close at this bar's close) is cancelled and placed anew.
    /// With `here` it fills at this bar's close (`OrderBroker::place_at_close`); an exit's leg is a
    /// leg of the exit its name is (`OrderBroker::place_exit_leg`). The index of its record.
    fn place(
        &mut self,
        name: &str,
        leg: TacticsBookLeg,
        lots: &str,
        group: Option<&str>,
        ticket: TacticsBookTicket,
        here: bool,
        hold: bool,
    ) -> usize {
        let epoch = self.book.opened.get(lots).copied().unwrap_or(0);
        let found =
            self.book.named.iter().position(|n| n.rests() && n.name == name && n.leg == leg);
        let mut kept_group = None;
        if let Some(i) = found {
            let n = &mut self.book.named[i];
            if !here && ticket.amends(&n.ticket) {
                if !n.unsent && !ticket.standalone() {
                    let (price, trigger) = ticket.amend_prices();
                    self.broker.amend(&n.tag, Some(ticket.size), price, trigger);
                }
                n.ticket = ticket;
                n.epoch = epoch;
                if group.is_some() {
                    n.group = group.map(str::to_string);
                }
                return i;
            }
            if !n.unsent {
                self.broker.cancel(&n.tag);
            }
            n.cancelled = true;
            kept_group = n.group.clone();
        }
        let tag = self.book.new_tag(name);
        let TacticsBookTicket { kind, side, size, reduce_only } = ticket;
        if hold {
            // sent at the end of the pass (`Tactics::send_held`)
        } else if here {
            self.broker.place_at_close(&self.book.symbol, &tag, side, size, reduce_only);
        } else if leg.is_guard() {
            self.broker.place_exit_leg(&self.book.symbol, name, &tag, side, size, kind);
        } else {
            self.broker.place(&self.book.symbol, &tag, side, size, kind, reduce_only);
        }
        self.book.named.push(TacticsBookSentOrder {
            tag,
            name: name.to_string(),
            leg,
            lots: lots.to_string(),
            group: group.map(str::to_string).or(kept_group),
            sizes: None,
            epoch,
            ticket,
            cancelled: false,
            unsent: hold,
        });
        self.book.named.len() - 1
    }

    /// Sends the entries this pass held back (`OrderBroker::rests_reduce_only_while_flat` is
    /// `false`): each alone, or with the exit legs issued for it in the same pass as ONE order list,
    /// the entry the parent and its legs OTO children cancelling one another (OCO). A held leg whose
    /// entry was cancelled in the pass is dropped with it, never sent.
    fn send_held(&mut self) {
        let entries: Vec<usize> = (0..self.book.named.len())
            .filter(|&i| {
                let n = &self.book.named[i];
                n.unsent && n.rests() && n.leg == TacticsBookLeg::Lot
            })
            .collect();
        for i in entries {
            let lots = self.book.named[i].lots.clone();
            let kids: Vec<usize> = (0..self.book.named.len())
                .filter(|&k| {
                    let n = &self.book.named[k];
                    n.unsent && n.rests() && n.leg.is_guard() && n.lots == lots
                })
                .collect();
            let entry = &self.book.named[i];
            if kids.is_empty() {
                let t = entry.ticket;
                let symbol = &self.book.symbol;
                self.broker.place(symbol, &entry.tag, t.side, t.size, t.kind, t.reduce_only);
            } else {
                let mut legs = vec![(entry.tag.clone(), list_leg(&entry.ticket, None, Vec::new()))];
                for (at, &k) in kids.iter().enumerate() {
                    let others = (1..=kids.len()).filter(|&j| j != at + 1).collect();
                    let n = &self.book.named[k];
                    legs.push((n.tag.clone(), list_leg(&n.ticket, Some(0), others)));
                }
                self.broker.place_group(&self.book.symbol, legs);
            }
            for k in std::iter::once(i).chain(kids) {
                self.book.named[k].unsent = false;
            }
        }
        for n in self.book.named.iter_mut().filter(|n| n.unsent) {
            n.unsent = false;
            n.cancelled = true;
        }
    }

    /// A level of an exit for a position `side` closes, entered at `entry`.
    fn level_of(level: Level, entry: f64, side: i32, take: bool) -> f64 {
        match level {
            Level::At(price) => price,
            Level::Percent(pct) if take => take_profit_price(entry, -side, entry * pct / 100.0),
            Level::Percent(pct) => stop_loss_price(entry, -side, entry * pct / 100.0),
        }
    }

    /// Protects the lots of `x.lots`, or an entry of that name still to come, from the next fill on.
    fn send_exit(&mut self, x: TacticsExit) {
        if !self.live || self.spent(&x.name) {
            return;
        }
        let (name, lots, group) = (x.name.as_str(), x.lots.as_str(), x.group.as_deref());
        let held = self.book.held_in(lots);
        let (side, whole, fresh) = if held > 0.0 {
            (closing_side(self.position()), held, false)
        } else {
            let coming = self
                .book
                .named
                .iter()
                .find(|n| n.rests() && n.leg == TacticsBookLeg::Lot && n.lots == lots);
            let Some(coming) = coming else { return };
            (-coming.ticket.side, coming.ticket.size, coming.unsent)
        };
        let want = x.size.map_or(whole, |s| s.min(whole));
        let size = self.claimed(name, lots, want, whole, group);
        self.book.dormant.retain(|d| d.name != name);
        let bound = self.book.bound_exits;
        if bound && size <= LOT_DUST {
            // The entry's other exits hold all of it: this one gets no order.
            self.pull_guard(name);
            return;
        }
        // Live (`oto`), an exit for an entry still to come never rests reduce-only while flat: sent
        // in this pass with its entry, its legs go as that entry's OTO children (`Tactics::send_held`);
        // once its entry was sent alone, it waits for the entry's fill here unless its legs already
        // rest as children.
        let children = held <= 0.0 && self.oto;
        let has_legs =
            self.book.named.iter().any(|n| n.rests() && n.name == name && n.leg.is_guard());
        if held <= 0.0 && (bound || x.relative() || (children && !fresh && !has_legs)) {
            self.wait_for_entry(name, lots, side, size, &x);
            return;
        }
        let hold = children && fresh;
        let entry = self.book.entry_price(lots).unwrap_or(f64::NAN);
        let take = x.take.map(|l| Self::level_of(l, entry, side, true));
        let cut = x.stop.map(|l| Self::level_of(l, entry, side, false));
        if bound && take.is_none() && cut.is_none() && x.trail.is_none() {
            let (name, lots, group) = (name.to_string(), lots.to_string(), x.group.clone());
            self.book.dormant.push(TacticsBookDormantExit { name, lots, size, group });
        }
        // A trail with no offset is an if-touched order, and is placed ahead of the target.
        let touch = x.trail.filter(|c| c.by_ticks == 0.0);
        if let Some(trail) = touch {
            self.rest_trail(name, lots, side, size, trail, group);
        }
        self.rest_levels(name, lots, (side, size), (take, cut), group, hold);
        match x.trail {
            Some(_) if touch.is_some() => {}
            Some(trail) => self.rest_trail(name, lots, side, size, trail, group),
            None => self.pull_leg(name, TacticsBookLeg::Trail),
        }
    }

    /// Whether the exit `name` has fired for the lots it covers. Under `bound_exits` it is live again
    /// once another entry has filled under those lots' name since (an exit is spent per entry fill).
    fn spent(&self, name: &str) -> bool {
        let book = &*self.book;
        let fresh = |lots: &str, opened: usize| {
            !book.bound_exits || book.opened.get(lots).copied().unwrap_or(0) == opened
        };
        book.spent.iter().any(|(exit, lots, opened)| exit == name && fresh(lots, *opened))
    }

    /// What the exit `name` takes of `lots`, which hold `whole`: `want`, less what the entry's other
    /// exits hold (`bound_exits`), each in the order they were issued.
    fn claimed(&self, name: &str, lots: &str, want: f64, whole: f64, group: Option<&str>) -> f64 {
        if !self.book.bound_exits {
            return want;
        }
        want.min(whole - self.book.reserved(name, lots, group)).max(0.0)
    }

    /// Takes the legs of the exit `name` off the book, and what it waits for or holds.
    fn pull_guard(&mut self, name: &str) {
        self.book.waiting.retain(|w| w.name != name);
        self.book.waiting_guards.retain(|w| w.name != name);
        self.book.dormant.retain(|d| d.name != name);
        self.book.pull_where(self.broker, |n| n.name == name && n.leg.is_guard());
    }

    /// An exit's stop and target, `(side, size)` each, the stop placed first; with `hold`, as OTO
    /// children of the entry this pass holds back.
    fn rest_levels(
        &mut self,
        name: &str,
        lots: &str,
        (side, size): (i32, f64),
        (take, cut): (Option<f64>, Option<f64>),
        group: Option<&str>,
        hold: bool,
    ) {
        let take = take.map(|p| {
            TacticsBookTicket::exit(
                OrderPlacement::Limit { price: self.limit_at(side, p) },
                side,
                size,
            )
        });
        let cut = cut.map(|p| {
            TacticsBookTicket::exit(
                OrderPlacement::Stop { trigger: self.stop_at(side, p) },
                side,
                size,
            )
        });
        for (leg, ticket) in [(TacticsBookLeg::Cut, cut), (TacticsBookLeg::Take, take)] {
            match ticket {
                Some(ticket) => {
                    self.place(name, leg, lots, group, ticket, false, hold);
                }
                None => self.pull_leg(name, leg),
            }
        }
    }

    fn pull_leg(&mut self, name: &str, leg: TacticsBookLeg) {
        self.book.pull_where(self.broker, |n| n.name == name && n.leg == leg);
    }

    /// An exit for an entry still to come rests no stop and no target: they are sent when that entry
    /// fills (`Tactics::arm_waiting`), sized from what it opened. Its trailing leg waits for the fill
    /// as every trailing leg issued with its entry does.
    fn wait_for_entry(&mut self, name: &str, lots: &str, side: i32, size: f64, x: &TacticsExit) {
        self.book.pull_where(self.broker, |n| n.name == name && n.leg.is_guard());
        self.book.waiting_guards.retain(|w| w.name != name);
        self.book.waiting_guards.push(TacticsBookWaitingGuard {
            name: name.to_string(),
            lots: lots.to_string(),
            size: x.size,
            claim: size,
            take: x.take,
            cut: x.stop,
            group: x.group.clone(),
        });
        match x.trail {
            Some(trail) => self.rest_trail(name, lots, side, size, trail, x.group.as_deref()),
            None => self.book.waiting.retain(|w| w.name != name),
        }
    }

    /// The stops and targets that waited for the entry of `lots`, sent now that it has filled at
    /// `price`. A leg the price is already past is sent alone, so the engine fills it where the walk
    /// goes on and its other leg does not fill as well.
    pub(crate) fn arm_waiting(&mut self, lots: &str, price: f64) {
        // One at a time, oldest first: those still waiting hold their claims while each is sized.
        while let Some(i) = self.book.waiting_guards.iter().position(|w| w.lots == lots) {
            let w = self.book.waiting_guards.remove(i);
            let held = self.book.held_in(&w.lots);
            if !self.live || held <= 0.0 || self.spent(&w.name) {
                continue;
            }
            let want = w.size.map_or(held, |s| s.min(held));
            let size = self.claimed(&w.name, &w.lots, want, held, w.group.as_deref());
            if size <= LOT_DUST {
                self.pull_guard(&w.name);
                continue;
            }
            let side = closing_side(self.position());
            let take = w.take.map(|l| Self::level_of(l, price, side, true));
            let cut = w.cut.map(|l| Self::level_of(l, price, side, false));
            let up = f64::from(side);
            let cut_past = cut.is_some_and(|p| up * (price - self.stop_at(side, p)) >= 0.0);
            let take_past = take.is_some_and(|p| up * (self.limit_at(side, p) - price) >= 0.0);
            let (take, cut) = match (cut_past, take_past) {
                (true, _) => (None, cut),
                (false, true) => (take, None),
                (false, false) => (take, cut),
            };
            let group = w.group.as_deref();
            self.rest_levels(&w.name, &w.lots, (side, size), (take, cut), group, false);
            // With no level and no trailing leg, the exit keeps holding its share of the entry.
            let trailed = self
                .book
                .named
                .iter()
                .any(|n| n.rests() && n.name == w.name && n.leg == TacticsBookLeg::Trail);
            if w.take.is_none() && w.cut.is_none() && !trailed {
                let (name, lots) = (w.name.clone(), w.lots.clone());
                self.book.dormant.push(TacticsBookDormantExit {
                    name,
                    lots,
                    size,
                    group: w.group.clone(),
                });
            }
        }
    }

    /// An exit's trailing leg. With no entry held yet it waits for the entry's fill. One with no
    /// offset whose level the mark has already reached leaves with the next market fill.
    fn rest_trail(
        &mut self,
        name: &str,
        lots: &str,
        side: i32,
        size: f64,
        trail: TacticsTrail,
        group: Option<&str>,
    ) {
        let Some(entry) = self.book.entry_price(lots) else {
            self.book.waiting.retain(|w| w.name != name);
            self.book.waiting.push(TacticsBookWaitingTrail {
                name: name.to_string(),
                lots: lots.to_string(),
                side,
                size,
                trail,
                group: group.map(str::to_string),
            });
            return;
        };
        if let Some(level) = touch_level(trail, entry, side, self.tick) {
            let mark = self.price();
            let reached = mark > 0.0 && trailing_activation_reached(side, mark, level);
            let ticket = if reached {
                TacticsBookTicket::exit(OrderPlacement::Market, side, size)
            } else {
                touch_ticket(level, side, size, self.setup.touch_at_market)
            };
            self.place(name, TacticsBookLeg::Trail, lots, group, ticket, false, false);
            return;
        }
        let activation = Some(activation(trail, entry, side, self.tick));
        let distance = trail.by_ticks * self.tick.unwrap_or(0.0);
        let same = OrderPlacement::Trailing { trail: distance, activation };
        let resting = self
            .book
            .named
            .iter()
            .position(|n| n.rests() && n.name == name && n.leg == TacticsBookLeg::Trail);
        if let Some(i) = resting {
            if self.book.named[i].ticket.kind == same {
                let n = &mut self.book.named[i];
                self.broker.amend(&n.tag, Some(size), None, None);
                n.ticket.size = size;
                return;
            }
            // Issued again with other values, it is a new trail: what the old one has seen goes.
            self.pull_leg(name, TacticsBookLeg::Trail);
        }
        let tag = self.book.new_tag(name);
        self.broker.place_exit_leg(&self.book.symbol, name, &tag, side, size, same);
        let epoch = self.book.opened.get(lots).copied().unwrap_or(0);
        self.book.named.push(TacticsBookSentOrder {
            tag,
            name: name.to_string(),
            leg: TacticsBookLeg::Trail,
            lots: lots.to_string(),
            group: group.map(str::to_string),
            sizes: None,
            epoch,
            ticket: TacticsBookTicket::exit(same, side, size),
            cancelled: false,
            unsent: false,
        });
    }
}
