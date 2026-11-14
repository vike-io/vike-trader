//! The language's own book: every order it has sent, by the TAG it sent it under, with the name,
//! the leg and the lots that order is for; the exits that have fired, wait for their entry or hold a
//! share with no level; the lots and bindings its fills made; and the fills-a-day cap.
//!
//! It learns what the order layer did through three doors only, each naming the order by its tag:
//! a fill (`vike_model::Fill::tag`), an order's end (a terminal `vike_model::OrderEvent`), and
//! what it sent itself. It never reads the engine's queue. An order it cancelled stays filed until
//! its end is heard or the next bar's settle, so a fill racing the cancel still finds the order that
//! made it; it counts as gone for everything else.
//!
//! A run keeps one book per symbol it trades ([`Books`]), each with its own lots, exits, tag numbers
//! and fills-a-day cap: an order's tag names its symbol's book.

use std::collections::HashMap;
use std::ops::{Deref, DerefMut};

use vike_model::{
    Fill, LOT_DUST, MS_PER_DAY, OrderBroker, OrderPlacement, closing_side, snap_limit,
};

use crate::lots::{self, TacticsBookBinding, TacticsBookLot, book_fill, closes_of};
use crate::tactics::Setup;
use crate::verbs::{Level, TacticsTrail};

/// One order as the language sends it through `vike_model::OrderBroker`: its kind and prices, its
/// side and size, and whether it may only reduce the position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TacticsBookTicket {
    pub(crate) kind: OrderPlacement,
    pub(crate) side: i32,
    pub(crate) size: f64,
    pub(crate) reduce_only: bool,
}

impl TacticsBookTicket {
    pub(crate) fn new(kind: OrderPlacement, side: i32, size: f64) -> TacticsBookTicket {
        TacticsBookTicket { kind, side, size, reduce_only: false }
    }

    pub(crate) fn exit(kind: OrderPlacement, side: i32, size: f64) -> TacticsBookTicket {
        TacticsBookTicket { reduce_only: true, ..TacticsBookTicket::new(kind, side, size) }
    }

    /// A pegged order or an iceberg: the seam amends neither (an amend of one is refused: cancel it
    /// and send it again), and an order list's leg carries neither a peg nor a visible size, so it is
    /// sent alone and replaced, never amended.
    pub(crate) fn standalone(&self) -> bool {
        matches!(self.kind, OrderPlacement::Pegged { .. } | OrderPlacement::Iceberg { .. })
    }

    /// Whether `old`, the order this one replaces, can take it by an amend: the same kind, side and
    /// reduce-only flag, an if-touched order with a limit or without one as before, and a trailing
    /// order with the same trail and activation (an amend moves only its size). A standalone one
    /// only when it is the same order again, which sends nothing.
    pub(crate) fn amends(&self, old: &TacticsBookTicket) -> bool {
        if self.standalone() || old.standalone() {
            return self == old;
        }
        let same_kind = match (self.kind, old.kind) {
            (
                OrderPlacement::TakeProfit { limit: a, .. },
                OrderPlacement::TakeProfit { limit: b, .. },
            ) => a.is_some() == b.is_some(),
            (
                OrderPlacement::Trailing { trail: a, activation: x },
                OrderPlacement::Trailing { trail: b, activation: y },
            ) => a == b && x == y,
            (a, b) => std::mem::discriminant(&a) == std::mem::discriminant(&b),
        };
        same_kind && self.side == old.side && self.reduce_only == old.reduce_only
    }

    /// The new prices an amend states, `(limit price, trigger)`, each `None` where the kind has none
    /// (`vike_model::modified_prices` puts each on its kind).
    pub(crate) fn amend_prices(&self) -> (Option<f64>, Option<f64>) {
        match self.kind {
            OrderPlacement::Market
            | OrderPlacement::Trailing { .. }
            | OrderPlacement::Pegged { .. } => (None, None),
            OrderPlacement::Limit { price } | OrderPlacement::Iceberg { price, .. } => {
                (Some(price), None)
            }
            OrderPlacement::Stop { trigger } => (None, Some(trigger)),
            OrderPlacement::StopLimit { trigger, limit } => (Some(limit), Some(trigger)),
            OrderPlacement::TakeProfit { trigger, limit } => (limit, Some(trigger)),
        }
    }
}

/// What an order is to the rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TacticsBookLeg {
    Lot,
    Drop,
    Take,
    Cut,
    Trail,
    Free,
}

impl TacticsBookLeg {
    pub(crate) fn is_guard(self) -> bool {
        matches!(self, TacticsBookLeg::Take | TacticsBookLeg::Cut | TacticsBookLeg::Trail)
    }
}

/// One order the language has sent.
#[derive(Debug, Clone)]
pub(crate) struct TacticsBookSentOrder {
    pub(crate) tag: String,
    pub(crate) name: String,
    pub(crate) leg: TacticsBookLeg,
    pub(crate) lots: String,
    pub(crate) group: Option<String>,
    /// An entry's `(asked, full)` sizes: `full` also carries the opposite position it meant to close.
    pub(crate) sizes: Option<(f64, f64)>,
    /// How many entries had filled under `lots` when this order was last issued (`TacticsBook::opened`).
    pub(crate) epoch: usize,
    /// What was sent, as amended since; its size is what the order still asks.
    pub(crate) ticket: TacticsBookTicket,
    /// A cancel was sent: it names only its own fill now.
    pub(crate) cancelled: bool,
    /// Not sent yet: an entry held to the end of its pass, so an exit issued with it can go as its
    /// OTO child (`Tactics::send_held`), or that child.
    pub(crate) unsent: bool,
}

impl TacticsBookSentOrder {
    pub(crate) fn rests(&self) -> bool {
        !self.cancelled
    }
}

/// A trailing leg issued with its entry: its level needs the fill price, so `TacticsBook::on_fill` sends it.
pub(crate) struct TacticsBookWaitingTrail {
    pub(crate) name: String,
    pub(crate) lots: String,
    pub(crate) side: i32,
    pub(crate) size: f64,
    pub(crate) trail: TacticsTrail,
    pub(crate) group: Option<String>,
}

/// The stop and target of an exit issued for an entry still to come (`crate::RuleRun::bound_exits`, or a
/// level in percent of that entry), sent when that entry fills.
pub(crate) struct TacticsBookWaitingGuard {
    pub(crate) name: String,
    pub(crate) lots: String,
    pub(crate) size: Option<f64>,
    /// What it holds of its entry while it waits, which the exits issued after it are sized against.
    pub(crate) claim: f64,
    pub(crate) take: Option<Level>,
    pub(crate) cut: Option<Level>,
    pub(crate) group: Option<String>,
}

/// An exit with no level, which holds its share of its entry all the same (`crate::RuleRun::bound_exits`).
pub(crate) struct TacticsBookDormantExit {
    pub(crate) name: String,
    pub(crate) lots: String,
    pub(crate) size: f64,
    pub(crate) group: Option<String>,
}

/// What the language remembers between hooks, for one symbol.
#[derive(Default)]
pub(crate) struct TacticsBook {
    /// The symbol its orders trade (`""`: the mount's own).
    pub(crate) symbol: String,
    /// What its tags start with: nothing on the run's own symbol (`<name>#<n>`), `<symbol>/` on
    /// another (`<symbol>/<name>#<n>`), so a tag names one order of the run whatever its symbol.
    prefix: String,
    pub(crate) named: Vec<TacticsBookSentOrder>,
    /// `(exit, lots, entries opened under the lots' name when it was last issued)`: an exit that has
    /// fired is not issued again while its lots live (under `bound_exits`, until another entry fills).
    pub(crate) spent: Vec<(String, String, usize)>,
    /// How many entries have filled under each lots name.
    pub(crate) opened: HashMap<String, usize>,
    pub(crate) waiting: Vec<TacticsBookWaitingTrail>,
    pub(crate) lots: Vec<TacticsBookLot>,
    /// Every entry still bound, oldest first. Kept on every run; read only under `bound_exits`.
    pub(crate) bindings: Vec<TacticsBookBinding>,
    pub(crate) waiting_guards: Vec<TacticsBookWaitingGuard>,
    pub(crate) dormant: Vec<TacticsBookDormantExit>,
    pub(crate) bound_exits: bool,
    /// The last tag number handed out.
    pub(crate) next: u64,
    fill_day: i64,
    fills_today: usize,
    /// The UTC day the fills-a-day cap has closed.
    pub(crate) capped_day: Option<i64>,
}

/// The level `trail` waits for: `after_ticks` past `entry` the way the position wants the price to
/// go. `side` closes the position.
pub(crate) fn activation(trail: TacticsTrail, entry: f64, side: i32, tick: Option<f64>) -> f64 {
    vike_model::take_profit_price(entry, -side, trail.after_ticks * tick.unwrap_or(0.0))
}

/// The snapped level a trail with no offset leaves at, `None` for one that trails.
pub(crate) fn touch_level(
    trail: TacticsTrail,
    entry: f64,
    side: i32,
    tick: Option<f64>,
) -> Option<f64> {
    (trail.by_ticks == 0.0).then(|| snap_limit(side, activation(trail, entry, side, tick), tick))
}

/// The if-touched order a trail with no offset rests as at `level`: with a limit there (a limit
/// fill), or with `at_market` a market once touched.
pub(crate) fn touch_ticket(level: f64, side: i32, size: f64, at_market: bool) -> TacticsBookTicket {
    let limit = (!at_market).then_some(level);
    TacticsBookTicket::exit(OrderPlacement::TakeProfit { trigger: level, limit }, side, size)
}

/// One [`TacticsBook`] per symbol a run trades, its own first (`crate::RuleRun::trades`), and which one is
/// CURRENT: a `Books` reads and writes as the current symbol's book (`crate::Tactics::on` moves it,
/// and every pass starts on the run's own).
pub(crate) struct Books {
    pub(crate) all: Vec<TacticsBook>,
    pub(crate) at: usize,
}

impl Deref for Books {
    type Target = TacticsBook;

    fn deref(&self) -> &TacticsBook {
        &self.all[self.at]
    }
}

impl DerefMut for Books {
    fn deref_mut(&mut self) -> &mut TacticsBook {
        &mut self.all[self.at]
    }
}

impl Books {
    /// The book of the run's own `symbol`, alone.
    pub(crate) fn new(symbol: &str) -> Books {
        Books {
            all: vec![TacticsBook { symbol: symbol.to_string(), ..TacticsBook::default() }],
            at: 0,
        }
    }

    /// The book of `symbol`: the run's own for `""` or its own symbol, else that symbol's, added on
    /// first use for one the run did not declare.
    pub(crate) fn add(&mut self, symbol: &str) -> usize {
        if symbol.is_empty() || symbol == self.all[0].symbol {
            return 0;
        }
        if let Some(at) = self.all.iter().position(|b| b.symbol == symbol) {
            return at;
        }
        let (prefix, bound_exits) = (format!("{symbol}/"), self.all[0].bound_exits);
        self.all.push(TacticsBook {
            symbol: symbol.to_string(),
            prefix,
            bound_exits,
            ..TacticsBook::default()
        });
        self.all.len() - 1
    }

    /// The book a fill of `symbol` is for: that symbol's, else the run's own when either is `""` (a
    /// single-symbol mount's "mine"), else none (not this run's).
    pub(crate) fn of_fill(&self, symbol: &str) -> Option<usize> {
        let own = self.all[0].symbol.is_empty() || symbol.is_empty();
        self.all.iter().position(|b| b.symbol == symbol).or(own.then_some(0))
    }
}

impl TacticsBook {
    /// A tag no other order of this run has had.
    pub(crate) fn new_tag(&mut self, name: &str) -> String {
        self.next += 1;
        format!("{}{name}#{}", self.prefix, self.next)
    }

    /// Whether the fills-a-day cap leaves the verbs acting at `ts` (`crate::RuleRun::fills_a_day`).
    pub(crate) fn uncapped(&self, ts: i64) -> bool {
        self.capped_day != Some(ts.div_euclid(MS_PER_DAY))
    }

    /// What the exits of `lots` other than `name` hold of them, each exit once, whether its legs
    /// rest, wait for their entry or have no level (`bound_exits`). The exits of one `group` are
    /// alternatives (one fill pulls the rest), so one of that group holds nothing against another.
    pub(crate) fn reserved(&self, name: &str, lots: &str, group: Option<&str>) -> f64 {
        let legs = self.named.iter().filter(|n| n.rests() && n.leg.is_guard() && n.lots == lots);
        let legs = legs.map(|n| (n.name.as_str(), n.ticket.size, n.group.as_deref()));
        let waiting = self.waiting_guards.iter().filter(|w| w.lots == lots);
        let trails = self.waiting.iter().filter(|w| w.lots == lots);
        let dormant = self.dormant.iter().filter(|d| d.lots == lots);
        let held = legs
            .chain(waiting.map(|w| (w.name.as_str(), w.claim, w.group.as_deref())))
            .chain(trails.map(|w| (w.name.as_str(), w.size, w.group.as_deref())))
            .chain(dormant.map(|d| (d.name.as_str(), d.size, d.group.as_deref())));
        let mut seen: Vec<&str> = Vec::new();
        let mut total = 0.0;
        for (exit, size, theirs) in held {
            let alternative = group.is_some() && theirs == group;
            if exit != name && !alternative && !seen.contains(&exit) {
                seen.push(exit);
                total += size;
            }
        }
        total
    }

    /// Cancels every order `gone` names that still rests (one not sent yet is only dropped).
    pub(crate) fn pull_where(
        &mut self,
        broker: &mut dyn OrderBroker,
        gone: impl Fn(&TacticsBookSentOrder) -> bool,
    ) {
        for n in &mut self.named {
            if n.rests() && gone(n) {
                if !n.unsent {
                    broker.cancel(&n.tag);
                }
                n.cancelled = true;
            }
        }
    }

    /// Whether the lots of `name` are held: what is bound to them under `bound_exits`, the lots
    /// themselves otherwise.
    pub(crate) fn holds(&self, name: &str) -> bool {
        if self.bound_exits {
            self.bindings.iter().any(|b| b.name == name)
        } else {
            self.lots.iter().any(|l| l.name == name)
        }
    }

    pub(crate) fn held_in(&self, name: &str) -> f64 {
        if self.bound_exits {
            self.bindings.iter().filter(|b| b.name == name).map(|b| b.left).sum()
        } else {
            self.lots.iter().filter(|l| l.name == name).map(|l| l.qty).sum()
        }
    }

    /// What the pyramiding limit counts.
    pub(crate) fn entries_held(&self) -> usize {
        if self.bound_exits { self.bindings.len() } else { self.lots.len() }
    }

    /// The fill price of the oldest held entry of `name`.
    pub(crate) fn entry_price(&self, name: &str) -> Option<f64> {
        if self.bound_exits {
            self.bindings.iter().find(|b| b.name == name).map(|b| b.price)
        } else {
            self.lots.iter().find(|l| l.name == name).map(|l| l.price)
        }
    }

    /// Takes `fill` off the bindings, after it is booked on the lots.
    fn bind(&mut self, fill: &Fill, closed: f64, own: Option<&str>, opens: &str) {
        let flat = self.lots.is_empty();
        if lots::bind(&mut self.bindings, fill, closed, own, opens, flat) {
            *self.opened.entry(opens.to_string()).or_insert(0) += 1;
        }
    }

    /// Whether an entry of `lots` is still to come.
    fn coming(&self, lots: &str) -> bool {
        self.named.iter().any(|m| m.rests() && m.leg == TacticsBookLeg::Lot && m.lots == lots)
    }

    /// An exit dies with its lots, unless an entry of that name is still to come.
    pub(crate) fn retire_guards(&mut self, broker: &mut dyn OrderBroker) {
        let spent = std::mem::take(&mut self.spent);
        self.spent = spent.into_iter().filter(|(_, name, _)| self.holds(name)).collect();
        let waiting = std::mem::take(&mut self.waiting);
        self.waiting = waiting.into_iter().filter(|w| self.coming(&w.lots)).collect();
        // One whose entry has just filled is sent by `RuleRun::on_fill` right after.
        let waiting = std::mem::take(&mut self.waiting_guards);
        self.waiting_guards =
            waiting.into_iter().filter(|w| self.coming(&w.lots) || self.holds(&w.lots)).collect();
        let dormant = std::mem::take(&mut self.dormant);
        self.dormant =
            dormant.into_iter().filter(|d| self.coming(&d.lots) || self.holds(&d.lots)).collect();
        let dead: Vec<String> = self
            .named
            .iter()
            .filter(|n| n.leg.is_guard() && !self.holds(&n.lots) && !self.coming(&n.lots))
            .map(|n| n.tag.clone())
            .collect();
        self.pull_where(broker, |n| dead.contains(&n.tag));
    }

    /// Before a rule runs: an order cancelled since the last bar is forgotten.
    pub(crate) fn settle(&mut self, broker: &mut dyn OrderBroker) {
        self.named.retain(TacticsBookSentOrder::rests);
        self.retire_guards(broker);
    }

    /// The order `tag` named has ended (a terminal lifecycle event); `true` when it was filed.
    pub(crate) fn end(&mut self, tag: &str) -> bool {
        let before = self.named.len();
        self.named.retain(|n| n.tag != tag);
        self.named.len() != before
    }

    /// Books `fill` of the order its tag names, and does what that fill sets off: a filled entry
    /// sends the trailing legs waiting for it, a filled exit is spent and pulls its other legs, a
    /// group member pulls the rest of its group, the resting entries are resized, and the
    /// fills-a-day cap counts it. `Some(lots)` when an entry of `lots` filled. A fill whose tag the
    /// book does not know (none: the engine's margin call) only closes lots. What it sends trades the
    /// book's symbol, with the run's cap and touch rule (`setup`).
    pub(crate) fn on_fill(
        &mut self,
        broker: &mut dyn OrderBroker,
        setup: &Setup,
        fill: &Fill,
        held: f64,
        tick: Option<f64>,
    ) -> Option<String> {
        let at = self.named.iter().position(|n| fill.tag.as_deref() == Some(n.tag.as_str()));
        let Some(at) = at else {
            self.on_cover(broker, fill, held);
            return None;
        };
        let n = &mut self.named[at];
        n.ticket.size -= fill.size;
        // nothing left to ask, or a reduce-only order with nothing left on the side it reduces
        let ended = n.ticket.size <= LOT_DUST
            || (n.ticket.reduce_only && held * f64::from(n.ticket.side) >= 0.0);
        let who = if ended { self.named.remove(at) } else { self.named[at].clone() };
        let closed = closes_of(&self.lots, fill);
        book_fill(&mut self.lots, fill, &who.lots);
        let own = (who.leg.is_guard() || who.leg == TacticsBookLeg::Drop) && !who.lots.is_empty();
        self.bind(fill, closed, own.then_some(who.lots.as_str()), &who.lots);
        if who.leg == TacticsBookLeg::Lot {
            self.arm_trails(broker, &who.lots, fill.price, (tick, setup.touch_at_market));
        }
        if who.leg.is_guard() {
            self.spent.push((who.name.clone(), who.lots.clone(), who.epoch));
            self.pull_where(broker, |n| n.tag != who.tag && n.name == who.name && n.leg.is_guard());
        }
        if let Some(group) = &who.group {
            self.pull_where(broker, |n| n.tag != who.tag && n.group.as_ref() == Some(group));
        }
        self.retire_guards(broker);
        self.resize_entries(broker, held);
        if let Some(cap) = setup.fills_a_day {
            self.count_against_cap(broker, fill, (cap, tick), held);
        }
        (who.leg == TacticsBookLeg::Lot).then_some(who.lots)
    }

    /// A fill no order of the book made (the engine's margin call names no tag): it only closes
    /// lots, and the exits of lots it emptied die. Not counted against the fills-a-day cap.
    fn on_cover(&mut self, broker: &mut dyn OrderBroker, fill: &Fill, held: f64) {
        let closed = closes_of(&self.lots, fill);
        book_fill(&mut self.lots, fill, "");
        self.bind(fill, closed, None, "");
        self.retire_guards(broker);
        self.resize_entries(broker, held);
    }

    /// What the fills-a-day cap does on the fill that reaches it: it cancels every order, closes what
    /// is held (`OrderBroker::place_close_from_fill`: live, a reduce-only market order; a simulator
    /// fills it from the fill's own price on the grid, `round(price / tick) * tick`), and the verbs do
    /// nothing until the UTC day ends.
    fn count_against_cap(
        &mut self,
        broker: &mut dyn OrderBroker,
        fill: &Fill,
        (cap, tick): (usize, Option<f64>),
        held: f64,
    ) {
        let day = fill.timestamp.div_euclid(MS_PER_DAY);
        if self.fill_day != day {
            self.fill_day = day;
            self.fills_today = 0;
        }
        self.fills_today += 1;
        if self.fills_today != cap {
            return;
        }
        self.capped_day = Some(day);
        self.waiting.clear();
        self.waiting_guards.clear();
        self.dormant.clear();
        self.pull_where(broker, |_| true);
        if held != 0.0 {
            let ticket =
                TacticsBookTicket::exit(OrderPlacement::Market, closing_side(held), held.abs());
            let tag = self.new_tag("cap close");
            let level = tick.map_or(fill.price, |tick| {
                vike_marketdata::TickFraction::new(tick).price_of((fill.price / tick).round())
            });
            broker.place_close_from_fill(&self.symbol, &tag, ticket.side, ticket.size, level);
            self.named.push(TacticsBookSentOrder {
                tag,
                name: "cap close".to_string(),
                leg: TacticsBookLeg::Drop,
                lots: String::new(),
                group: None,
                sizes: None,
                epoch: 0,
                ticket,
                cancelled: false,
                unsent: false,
            });
        }
    }

    /// Sends the trailing legs that waited for this entry, which filled at `entry`.
    fn arm_trails(
        &mut self,
        broker: &mut dyn OrderBroker,
        lots: &str,
        entry: f64,
        (tick, touch_at_market): (Option<f64>, bool),
    ) {
        let (now, later): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.waiting).into_iter().partition(|w| w.lots == lots);
        self.waiting = later;
        for w in now {
            let ticket = match touch_level(w.trail, entry, w.side, tick) {
                Some(level) => touch_ticket(level, w.side, w.size, touch_at_market),
                None => {
                    let activation = Some(activation(w.trail, entry, w.side, tick));
                    let trail = w.trail.by_ticks * tick.unwrap_or(0.0);
                    TacticsBookTicket::exit(
                        OrderPlacement::Trailing { trail, activation },
                        w.side,
                        w.size,
                    )
                }
            };
            let tag = self.new_tag(&w.name);
            let (symbol, exit) = (&self.symbol, &w.name);
            broker.place_exit_leg(symbol, exit, &tag, ticket.side, ticket.size, ticket.kind);
            let epoch = self.opened.get(&w.lots).copied().unwrap_or(0);
            self.named.push(TacticsBookSentOrder {
                tag,
                name: w.name,
                leg: TacticsBookLeg::Trail,
                lots: w.lots,
                group: w.group,
                sizes: None,
                epoch,
                ticket,
                cancelled: false,
                unsent: false,
            });
        }
    }

    /// While the position is on a resting entry's own side there is nothing for it to close, so it
    /// asks only what it asked; otherwise its full size. An entry of size 0, there only to close the
    /// position it faced, is cancelled once there is nothing for it to close. A standalone entry
    /// (`TacticsBookTicket::standalone`) is cancelled and placed anew at its new size, under a new tag.
    fn resize_entries(&mut self, broker: &mut dyn OrderBroker, held: f64) {
        let mut renewed = Vec::new();
        for n in self.named.iter_mut().filter(|n| n.rests()) {
            let Some((asked, full)) = n.sizes else { continue };
            let size =
                if held != 0.0 && (held > 0.0) == (n.ticket.side > 0) { asked } else { full };
            if n.ticket.size != size {
                n.ticket.size = size;
                if n.unsent {
                    continue;
                }
                if n.ticket.standalone() {
                    broker.cancel(&n.tag);
                    n.cancelled = true;
                    if size > 0.0 {
                        renewed.push(n.clone());
                    }
                } else {
                    broker.amend(&n.tag, Some(size), None, None);
                }
            }
        }
        for mut n in renewed {
            n.tag = self.new_tag(&n.name);
            n.cancelled = false;
            let t = n.ticket;
            broker.place(&self.symbol, &n.tag, t.side, t.size, t.kind, t.reduce_only);
            self.named.push(n);
        }
        self.pull_where(broker, |n| n.ticket.size <= 0.0);
    }
}
