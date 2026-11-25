//! [`RuleRun`], the ONE wrapper that makes a [`Rule`] a `vike_model::Strategy<B>` for every broker that
//! implements the portable order seam (`vike_model::OrderBroker`): the simulator's `OrderBrokerSimulator` in a
//! backtest, the live core's `LiveBroker` on paper and live. It feeds the language's book from the
//! three doors the seam has (a bar, a fill naming its tag, an order's end) and runs the rule.
//!
//! A run trades its own symbol and those it declares ([`RuleRun::trades`]), one book each: a fill goes
//! to its symbol's book, an order's end to the book its tag is filed in.

use vike_marketdata::Bar;
use vike_model::{Fill, OrderBroker, OrderEvent, Strategy};

use crate::book::Books;
use crate::header::RuleDefaults;
use crate::rule::Rule;
use crate::tactics::{Setup, Tactics};

/// A rule mounted on its own symbol (and those it declares, [`RuleRun::trades`]), with a book for
/// each. Built with [`RuleRun::new`] and the builder options below; each switch is OFF by default.
pub struct RuleRun<R> {
    rule: R,
    setup: Setup,
    books: Books,
}

/// The four switches of [`RuleRun`] as one value a table can carry (a strategy gallery row's), each the
/// [`RuleRun`] option of its name; [`RuleRunOptions::OFF`] is a run's defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuleRunOptions {
    /// [`RuleRun::bound_exits`].
    pub bound_exits: bool,
    /// [`RuleRun::stacked_closes`].
    pub stacked_closes: bool,
    /// [`RuleRun::fills_a_day`], `None` for no cap.
    pub fills_a_day: Option<usize>,
    /// [`RuleRun::touch_at_market`].
    pub touch_at_market: bool,
}

impl RuleRunOptions {
    /// Every switch off, as [`RuleRun::new`] builds a run.
    pub const OFF: RuleRunOptions = RuleRunOptions {
        bound_exits: false,
        stacked_closes: false,
        fills_a_day: None,
        touch_at_market: false,
    };
}

impl<R: Rule> RuleRun<R> {
    /// `rule` trading `symbol`, with the rule's own header.
    pub fn new(rule: R, symbol: &str) -> Self {
        let setup = Setup {
            header: rule.defaults(),
            stacked_closes: false,
            fills_a_day: None,
            touch_at_market: false,
            slippage: 0.0,
            qty_step: None,
            tick: None,
            window: None,
        };
        RuleRun { rule, setup, books: Books::new(symbol) }
    }
}

impl<R> RuleRun<R> {
    /// The symbols it trades beside its own, which a rule reaches through [`crate::Tactics::on`]:
    /// each keeps a book of its own, and its orders are tagged `<symbol>/<name>#<n>` (the own
    /// symbol's stay `<name>#<n>`). The run's own symbol, or one named twice, is one book.
    pub fn trades<S: AsRef<str>>(mut self, symbols: &[S]) -> Self {
        for symbol in symbols {
            self.books.add(symbol.as_ref());
        }
        self
    }

    /// The header the run's verbs take instead of the rule's (a run's own override).
    pub fn defaults(mut self, header: RuleDefaults) -> Self {
        self.setup.header = header;
        self
    }

    /// The tick levels on the run's own symbol snap to and a trail counts in. Without it, the
    /// broker's grid for the symbol (`vike_model::OrderBroker::instrument_grid`), which is also what
    /// every other symbol takes ([`RuleRun::trades`]); with neither, levels are sent as stated and a
    /// trail is zero.
    pub fn tick(mut self, tick: f64) -> Self {
        self.setup.tick = Some(tick);
        self
    }

    /// The verbs act on bars from `from_ms` up to, NOT including, `to_ms`: the window's last bar,
    /// on which only what a fill inside it makes the rule issue acts (its own walk is still to come).
    /// Without it they act on every bar.
    pub fn trading(mut self, from_ms: i64, to_ms: i64) -> Self {
        self.setup.window = Some((from_ms, to_ms));
        self
    }

    /// An exit belongs to the entry it was issued for: it sizes from, and dies with, what is still
    /// bound to the entries of its lots' name ([`crate::TacticsBookBinding`]), not what first-in first-out has
    /// left of their lots; one issued for an entry still to come waits for that entry's fill; a
    /// close of a name closes what is bound to it; the pyramiding limit counts the entries still
    /// bound; and the exits of one entry share its quantity in the order they were issued, one that
    /// finds nothing left getting no order and one with no level holding its share all the same.
    /// Every fill still closes the oldest lots first.
    pub fn bound_exits(mut self, on: bool) -> Self {
        self.books.all.iter_mut().for_each(|book| book.bound_exits = on);
        self
    }

    /// Each close is an order of its own, so two closes of one name issued in one pass both fill.
    /// Without it the later replaces the earlier.
    pub fn stacked_closes(mut self, on: bool) -> Self {
        self.setup.stacked_closes = on;
        self
    }

    /// The most fills in one UTC day, counted per symbol: the fill that reaches it cancels every
    /// order of its symbol and closes what is held of it, and no verb acts on that symbol again
    /// until the day ends.
    pub fn fills_a_day(mut self, cap: usize) -> Self {
        self.setup.fills_a_day = Some(cap);
        self
    }

    /// A trail with no offset rests as a market once its level is touched, not as a limit there.
    pub fn touch_at_market(mut self, on: bool) -> Self {
        self.setup.touch_at_market = on;
        self
    }

    /// The four switches at once, as `options` states them (a cap of `None` lifts one set before).
    pub fn with(mut self, options: RuleRunOptions) -> Self {
        let RuleRunOptions { bound_exits, stacked_closes, fills_a_day, touch_at_market } = options;
        self.books.all.iter_mut().for_each(|book| book.bound_exits = bound_exits);
        self.setup.stacked_closes = stacked_closes;
        self.setup.fills_a_day = fills_a_day;
        self.setup.touch_at_market = touch_at_market;
        self
    }

    /// The four switches this run holds.
    pub fn options(&self) -> RuleRunOptions {
        RuleRunOptions {
            bound_exits: self.books.all[0].bound_exits,
            stacked_closes: self.setup.stacked_closes,
            fills_a_day: self.setup.fills_a_day,
            touch_at_market: self.setup.touch_at_market,
        }
    }

    /// What a market or stop fill on the run's own symbol is expected to pay over its price, in the
    /// quote, which [`crate::TacticsSizing::slipped`] adds against the trader. A read for a size only: the
    /// broker charges what it charges. Without it, none, as on every other symbol.
    pub fn slippage(mut self, amount: f64) -> Self {
        self.setup.slippage = amount;
        self
    }

    /// The quantity step a size on the run's own symbol worked out from money floors to
    /// ([`crate::TacticsSizing`]). Without it, the lot size of the broker's grid for the symbol
    /// (`vike_model::OrderBroker::instrument_grid`), which is also what every other symbol takes; with
    /// neither, sizes are not floored. A step that is not a positive number floors nothing,
    /// whatever the grid states.
    pub fn qty_step(mut self, step: f64) -> Self {
        self.setup.qty_step = Some(step);
        self
    }

    /// The rule.
    pub fn rule(&self) -> &R {
        &self.rule
    }

    /// The rule, to change what it holds between hooks.
    pub fn rule_mut(&mut self) -> &mut R {
        &mut self.rule
    }

    /// The name of the lots the order behind `fill` opens or closes (`""` for a fill no order of
    /// this run made). Read BEFORE the fill reaches the run, which may forget the order.
    pub fn lot_name(&self, fill: &Fill) -> &str {
        let mut named = self.books.all.iter().flat_map(|book| &book.named);
        let named = named.find(|n| fill.tag.as_deref() == Some(n.tag.as_str()));
        named.map_or("", |n| n.lots.as_str())
    }

    /// Whether `ts` is inside the run's trading window ([`RuleRun::trading`]).
    fn open_at(&self, ts: i64) -> bool {
        self.setup.window.is_none_or(|(from, to)| ts >= from && ts < to)
    }

    /// One pass of the verbs OUTSIDE a bar, at `ts`: `hook` reads and issues through [`Tactics`] as
    /// [`Rule::on_bar`] does, and what it issued is lowered when it returns. For a front end whose
    /// own hooks the seam reaches between bars (a Rhai script's `on_tick` and `on_stop`). Nothing
    /// settles here: an order that ended is still forgotten at the next bar.
    pub fn pass<B: OrderBroker>(
        &mut self,
        broker: &mut B,
        ts: i64,
        hook: impl FnOnce(&mut R, &mut Tactics<'_>),
    ) {
        let broker: &mut dyn OrderBroker = broker;
        let open = self.open_at(ts);
        let mut strategy = Tactics::new(broker, &mut self.books, &self.setup, 0, open, ts);
        hook(&mut self.rule, &mut strategy);
        strategy.finish();
    }
}

impl<R: Rule> RuleRun<R> {
    /// `Strategy::on_fill` with the bar under way stated: `bar` is what [`Rule::after_fill`] reads.
    /// While a bar's fills are delivered the seam shows only the bars closed before it
    /// (`vike_model::OrderBroker::bars`), so `Strategy::on_fill` hands the rule the last of those; a
    /// runner that holds the bar being walked passes it here instead. A fill is INSIDE the bar under
    /// way when it is stamped after the last closed bar (a fill at a close is stamped with that bar).
    /// The lots match by SYMBOL, as the closed-trade ledger's do: a fill goes to its symbol's book,
    /// and one of a symbol the run does not trade is not this rule's (an empty symbol on either side
    /// is a single-symbol mount's "mine"). [`Rule::after_fill`] starts on the run's own symbol, as
    /// every hook does.
    pub fn on_fill_within<B: OrderBroker>(&mut self, broker: &mut B, fill: &Fill, bar: &Bar) {
        let Some(at) = self.books.of_fill(&fill.symbol) else { return };
        let broker: &mut dyn OrderBroker = broker;
        let book = &mut self.books.all[at];
        let tick = self.setup.tick_of(|| broker.instrument_grid(&book.symbol), at == 0);
        let held = broker.position(&book.symbol);
        // a fill no order of the book made (the engine's margin call) only closes lots
        let ours = book.named.iter().any(|n| fill.tag.as_deref() == Some(n.tag.as_str()));
        let opened = book.on_fill(broker, &self.setup, fill, held, tick);
        if !ours {
            return;
        }
        let closed = broker.bars(&book.symbol).last().map(|b| b.timestamp);
        let inside = closed.is_some_and(|ts| fill.timestamp > ts);
        let waited = opened.filter(|lots| book.waiting_guards.iter().any(|w| w.lots == *lots));
        if !inside && waited.is_none() {
            return;
        }
        let last = self.setup.window.is_some_and(|(_, to)| fill.timestamp == to);
        let open = self.open_at(fill.timestamp) || (inside && last);
        let mut strategy =
            Tactics::new(broker, &mut self.books, &self.setup, at, open, fill.timestamp);
        if let Some(lots) = waited {
            strategy.arm_waiting(&lots, fill.price);
        }
        if inside {
            self.rule.after_fill(strategy.on(""), bar);
        }
        strategy.finish();
    }
}

impl<B: OrderBroker, R: Rule> Strategy<B> for RuleRun<R> {
    /// The rule's own ([`Rule::inputs`]): a wrapper that drops them leaves the rule reading nothing.
    fn inputs(&self) -> Vec<String> {
        self.rule.inputs()
    }

    /// The symbols [`RuleRun::trades`] declared beside the own: a live mount declares each as a leg.
    fn symbols(&self) -> Vec<String> {
        self.books.all[1..].iter().map(|book| book.symbol.clone()).collect()
    }

    /// The run's header commission ([`vike_model::Commission::schedule`]): a backtest charges it when
    /// its profile states none.
    fn stated_fee(&self) -> Option<vike_model::FeeSchedule> {
        self.setup.header.fee.schedule()
    }

    /// The run's header slippage ([`vike_model::Slippage::table`]): a backtest charges it when
    /// the run states none.
    fn stated_slippage(&self) -> Option<vike_model::ProfileEngineSlippage> {
        self.setup.header.slippage.table()
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let broker: &mut dyn OrderBroker = broker;
        for book in &mut self.books.all {
            book.settle(broker);
        }
        let open = self.open_at(bar.timestamp);
        let mut strategy =
            Tactics::new(broker, &mut self.books, &self.setup, 0, open, bar.timestamp);
        self.rule.on_bar(&mut strategy, bar);
        strategy.finish();
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        let Some(at) = self.books.of_fill(&fill.symbol) else { return };
        // with no bar closed yet nothing is inside one, so the rule does not run on this stand-in
        let last = broker.bars(&self.books.all[at].symbol).last().cloned().unwrap_or(Bar {
            timestamp: fill.timestamp,
            open: fill.price,
            high: fill.price,
            low: fill.price,
            close: fill.price,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: None,
        });
        self.on_fill_within(broker, fill, &last);
    }

    /// The end of one of the run's orders (cancelled, refused, expired, or filled whole) takes it
    /// off the book, and an exit waiting for an entry that ended with it dies too.
    fn on_order_event(&mut self, broker: &mut B, event: &OrderEvent) {
        let Some(tag) = event.tag.as_deref().filter(|_| event.kind.is_terminal()) else { return };
        if let Some(at) = self.books.all.iter_mut().position(|book| book.end(tag)) {
            self.books.all[at].retire_guards(broker);
        }
    }
}
