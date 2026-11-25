//! The `strategy` object (decision 0269's Rhai wave): `vike-tactics`' order language, spelled in a
//! script as a Rust rule spells it. A script opts in by its bar hook's one parameter, named
//! `strategy`; [`crate::RhaiStrategy`] then runs it inside `vike_tactics::RuleRun`, the one wrapper the
//! lab's rules run in, so a rule's orders are the same whichever front end wrote them.
//!
//! ```text
//! let fast = param("fast", 5.0);
//! fn defaults() { #{ size: 1.0, pyramiding: 1 } }   // optional; `RuleDefaults::PLAIN` without it
//! fn on_bar(strategy) {
//!     let f = sma(fast.to_int());
//!     if f > close() && strategy.position() <= 0.0 {
//!         strategy.entry("L1", OrderSide::Long);
//!         strategy.exit("L1").stop_pct(1.0).take_pct(2.0);
//!     }
//!     if f < close() { strategy.close("L1"); }
//! }
//! fn after_fill(strategy) { }                        // optional: `Rule::after_fill`
//! fn on_tick(strategy) { }                           // optional: a live quote, from the first bar
//! fn on_stop(strategy) { }                           // optional: reads only
//! ```
//!
//! The verbs and their builders are `Tactics`' and `vike_tactics::{TacticsEntry, TacticsOrder, TacticsClose, TacticsExit}`'s:
//! `entry`/`order(name, OrderSide::Long | OrderSide::Short)` with `.size .stop .limit .chase .iceberg(visible)
//! .group` (`.chase()` pegs the order to its own side of the touch; `.iceberg(visible)` shows only
//! `visible` of a limit's size; terms no kind states, a `visible` not above zero among them, are
//! refused by name as the language refuses them, a `Denied` order event);
//! `close(name)`/`close_all()` with `.size .here`; `exit(name)` with `.from .size .stop .take
//! .stop_pct .take_pct .trail(#{ after_ticks, by_ticks }) .group`; `cancel(name)`, `cancel_all()`;
//! `forecast(value, horizon_ms)` and `forecast(value, horizon_ms, confidence)` publish a forecast for
//! the current symbol (book W35, `Tactics::forecast`): no order moves, a backtest keeps it with the
//! run, and a term `vike_model::Forecast::new` refuses is the hook's error.
//! The reads: `position()`, `avg_cost()`, `equity()`, `price()`, `lots()` (maps of `name side qty
//! price entry_timestamp`), `snap_stop(side, price)`, `snap_limit(side, price)`.
//!
//! **Several symbols** (`vike_tactics::Tactics::on`): a script names the symbols it trades beside
//! its own in `fn trades() { ["ETHUSDT"] }`, read once at compile ([`trades_of`]). `strategy.on(symbol)`
//! makes one current until the next `on` or the hook's end, and returns `strategy`, so
//! `strategy.on("ETHUSDT").entry("L", OrderSide::Long)` trades it: every verb and read after it
//! (`position()`, `avg_cost()`, `price()`, `lots()`, the snaps) is that symbol's, each hook starting on
//! the run's own (`""` or its own symbol names it). `position_of(symbol)` and `price_of(symbol)` read
//! any of them without moving. A symbol `fn trades()` does not declare is an error of the hook, by
//! name: its reads are taken as the hook starts, so only a declared one has any. ⚠ A backtest verb
//! for now: live, a mount that declares no symbols refuses every order on another symbol
//! (`vike_tactics::Tactics::on`).
//!
//! **Inputs** (decision 0271): a stored series beside the run's own bars, read by name as of now,
//! the bar in progress never. A script names the ones it reads in `fn inputs() { ["1d",
//! "binance:ETHUSDT:1h"] }`, read once at compile ([`inputs_of`]) and handed to the run as
//! `vike_model::Strategy::inputs`, which loads only those. `strategy.input(name)` is an `Input`
//! whose `close() open() high() low() volume()` are its last closed bar's, `time()` that bar's OPEN
//! time in epoch ms; `bars(n)` is the last `n` closed bars and `since(ts)` the closed bars opened at
//! or after `ts`, each an array of maps `#{ time, open, high, low, close, volume }`, oldest first.
//! ⚠ **No value is `()`, never a NaN**: before the input's first bar closes, and for a name
//! `fn inputs()` does not declare, `close()` and the other five are `()` and `bars`/`since` empty.
//! Test it (`if c == () { return; }`): arithmetic on `()` errors the hook.
//!
//! **Closed trades**: `closed_trades()` is the run's closed-trade ledger so far, as the broker
//! booked it (`vike_tactics::Tactics::closed_trades`, never a second booking), one map per row in
//! closing order: `symbol tag side qty entry_timestamp entry_bar entry_price exit_timestamp exit_bar exit_price
//! pnl commission net` (`vike_model::TradeClosedRow`'s fields; `side` is `OrderSide::Long`/`OrderSide::Short`).
//! `closed_net()` is their net (`Tactics::closed_net`). Both are `()` where the broker keeps no
//! ledger (a paper or live mount today), which is not "nothing closed": the simulator keeps one,
//! `[]` and `0.0` before the first close. One array of a script holds at most 682 bars or 315 closed
//! trades (`MAX_BARS`, `MAX_TRADES`: the sandbox counts every field of every map in it against its
//! limit of 4096): `bars(n)` refuses an `n` past that, and `since` and `closed_trades` a longer answer,
//! each by name with the read to use instead. **The index reads walk any length, one map an answer**:
//! `input.count()` and `input.bar(i)`, `strategy.closed_count()` and `strategy.closed_trade(i)`, OLDEST
//! FIRST (`0` the oldest, `-1` the newest), `()` outside the list or where the broker keeps no ledger.
//!
//! **How a call reaches the language.** A Rhai value cannot hold the `vike_tactics::Tactics` the
//! hook borrows, so the object RECORDS: each verb is one [`Call`], each builder method one option of
//! the call it chains onto, in call order, and when the hook returns [`replay`] issues them on the
//! real `Tactics` in that order. That is the language's own order: a builder only records, and its
//! order is lowered when the next verb starts or the hook ends. The reads are taken when the hook
//! starts ([`snapshot`]): they change only on fills (`Tactics`' module doc), and no fill lands inside
//! a hook. An input's bars and the closed trades only grow at their end, so the snapshot appends what
//! is new ([`follow`]) rather than copying a whole history per hook. A hook that errors issues nothing. An option chained onto a builder after a later verb is
//! an error: the borrow Rust refuses.
//!
//! **The other hooks**, where the seam is valid. `fn on_tick(strategy)` runs on a quote, sampled as
//! `fn on_tick()` is (`crate::TICK_HOOK_MIN_MS`), from the run's first bar on (the bar names the
//! symbol it trades; before it a quote runs nothing), in a pass of `vike_tactics::RuleRun::pass`.
//! `fn on_stop(strategy)` only READS: the strategy is stopping (a backtest is over, a live mount is
//! coming down), so each verb there is refused by its name. `fn on_start(strategy)` is refused at
//! compile: before the first bar the run has no symbol and no price. Each takes `strategy` only
//! beside `fn on_bar(strategy)`, and a hook is defined one way. The zero-argument
//! `on_start`/`on_tick`/`on_stop` run beside it as before, on the global reads. The strategy object
//! is a script's one door for an order: the order functions (`buy`, `sell`, `market`, `limit`) were
//! removed on 2026-11-10, and a script that still calls one is refused at compile with the new
//! spelling (`crate::RhaiStrategy`'s module doc).
//!
//! Not here: the run's options (`RuleRun::bound_exits` and its siblings stay off, their default),
//! `Tactics::bindings`, and a header's commission (nothing outside the lab reads one yet).

use vike_marketdata::Bar;
use vike_model::OrderSide;
use vike_model::{Slippage, TradeClosedRow, snap_limit, snap_stop};
use vike_tactics::{RuleDefaults, Tactics, TacticsBookLot, TacticsTrail};

use crate::context::SharedContext;
use crate::engine::host::arg;

type Fail = Box<rhai::EvalAltResult>;

/// The most bars and the most closed trades one answer of `bars`, `since` and `closed_trades` holds: the
/// sandbox (`SandboxLimits::SCRIPT`) counts every field of every map in an array against its map limit
/// (4096), so an array of [`bar_map`]s (6 fields each) holds 682 of them, and of [`trade_map`]s (13
/// fields) 315. A longer answer is refused by name, with the read to use instead, never left to the
/// sandbox's own size error.
const MAX_BARS: usize = max_items(BAR_FIELDS);
const MAX_TRADES: usize = max_items(TRADE_FIELDS);
const BAR_FIELDS: usize = 6;
const TRADE_FIELDS: usize = 13;

/// How many maps of `fields` fields one array may hold within the sandbox's map and array limits.
const fn max_items(fields: usize) -> usize {
    let limits = crate::engine::host::SandboxLimits::SCRIPT;
    let by_fields = limits.max_map_size / fields;
    if by_fields < limits.max_array_size { by_fields } else { limits.max_array_size }
}

/// One verb a script called on `strategy`.
#[derive(Debug, Clone, PartialEq)]
enum Verb {
    Entry(String, OrderSide),
    Order(String, OrderSide),
    /// `None`: `close_all`.
    Close(Option<String>),
    Exit(String),
    Cancel(String),
    CancelAll,
    /// `strategy.on(symbol)`: sends nothing, so `on_stop` records it too.
    On(String),
    /// `strategy.forecast(value, horizon_ms[, confidence])` (book W35), judged by `Forecast::new` when it
    /// is called.
    Forecast(vike_model::Forecast),
}

impl Verb {
    /// Its name on `strategy`, for a refusal.
    fn name(&self) -> &'static str {
        match self {
            Verb::On(_) => "on",
            Verb::Entry(..) => "entry",
            Verb::Order(..) => "order",
            Verb::Close(Some(_)) => "close",
            Verb::Close(None) => "close_all",
            Verb::Exit(_) => "exit",
            Verb::Cancel(_) => "cancel",
            Verb::CancelAll => "cancel_all",
            Verb::Forecast(_) => "forecast",
        }
    }
}

/// Every name `register` binds on the `strategy` object and its builders: the verbs, the
/// builders' options, the reads. A shell describes them from this roster
/// (`crates/vike-cli/src/cmd/backtest/strategies.rs`'s `HOST_FUNCTIONS`, held to it both ways). ⚠ A name added to
/// `register` is added here too: `every_strategy_fn_name_is_callable` catches a stale entry, not an
/// unlisted new one (rhai lists no registrations without its `metadata` feature).
pub const STRATEGY_FN_NAMES: &[&str] = &[
    "entry",
    "order",
    "close",
    "close_all",
    "exit",
    "cancel",
    "cancel_all",
    "forecast",
    "on",
    "size",
    "stop",
    "limit",
    "chase",
    "iceberg",
    "group",
    "here",
    "from",
    "take",
    "stop_pct",
    "take_pct",
    "trail",
    "position",
    "position_of",
    "avg_cost",
    "equity",
    "price",
    "price_of",
    "lots",
    "snap_stop",
    "snap_limit",
    "input",
    "open",
    "high",
    "low",
    "volume",
    "time",
    "bars",
    "since",
    "count",
    "bar",
    "closed_trades",
    "closed_net",
    "closed_count",
    "closed_trade",
];

/// One builder method, chained onto the verb before it.
#[derive(Debug, Clone, PartialEq)]
enum Opt {
    Size(f64),
    Stop(f64),
    Limit(f64),
    Chase,
    Iceberg(f64),
    Group(String),
    Here,
    From(String),
    Take(f64),
    StopPercent(f64),
    TakePercent(f64),
    Trail(TacticsTrail),
}

/// A verb and the options chained onto it, in call order.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Call {
    verb: Verb,
    opts: Vec<Opt>,
}

/// One symbol's reads, taken as a hook starts.
#[derive(Debug, Clone, Default)]
struct Held {
    position: f64,
    avg_cost: f64,
    price: f64,
    tick: Option<f64>,
    lots: Vec<TacticsBookLot>,
}

/// What the reads answer for one hook, taken from `Tactics` as it starts.
#[derive(Debug, Clone)]
pub(crate) struct Reads {
    /// The run's own symbol's reads (named by the first snapshot), then each symbol `fn trades()`
    /// declares ([`Reads::trading`]).
    symbols: Vec<(String, Held)>,
    /// Where in `symbols` the current symbol is (`strategy.on`); each hook starts on the own, `0`.
    at: usize,
    equity: f64,
    /// Each input `fn inputs()` declares, and its closed bars, oldest first (seeded empty at compile).
    pub(crate) inputs: Vec<(String, Vec<Bar>)>,
    /// The closed-trade ledger's rows and their net; `None` where the broker keeps no ledger.
    closed: Option<(Vec<TradeClosedRow>, f64)>,
}

impl Default for Reads {
    /// The run's own symbol alone, nothing read yet.
    fn default() -> Self {
        let own = (String::new(), Held::default());
        Reads { symbols: vec![own], at: 0, equity: 0.0, inputs: Vec::new(), closed: None }
    }
}

impl Reads {
    /// Adds the symbols `fn trades()` declares (`trades_of`) to those a hook reads.
    pub(crate) fn trading(&mut self, trades: &[String]) {
        self.symbols.extend(trades.iter().map(|s| (s.clone(), Held::default())));
    }

    /// The current symbol's reads.
    fn held(&self) -> &Held {
        &self.symbols[self.at].1
    }

    /// Where `symbol` is in `symbols`: `""` and the run's own symbol are its own, `0`.
    fn place_of(&self, symbol: &str) -> Option<usize> {
        if symbol.is_empty() {
            return Some(0);
        }
        self.symbols.iter().position(|(s, _)| s == symbol)
    }
}

/// A symbol `fn trades()` does not declare, named in `strategy.<verb>(..)`: the hook's error.
fn undeclared(verb: &str, symbol: &str) -> Fail {
    format!(
        "strategy.{verb}(\"{symbol}\"): a script trades its own symbol and those fn trades() \
         declares, fn trades() {{ [\"{symbol}\"] }}"
    )
    .into()
}

/// The `strategy` value a hook receives (Rhai type `Tactics`).
#[derive(Clone)]
pub(crate) struct Handle(pub(crate) SharedContext);

/// What `strategy.input(name)` answers (Rhai type `Input`): that input's closed bars as the hook started.
#[derive(Clone)]
struct InputHandle(SharedContext, String);

impl InputHandle {
    /// `f` over the input's closed bars, oldest first: none for a name `fn inputs()` does not declare.
    fn bars<T>(&self, f: impl FnOnce(&[Bar]) -> T) -> T {
        let g = self.0.read().unwrap();
        let held = g.reads.inputs.iter().find(|(name, _)| *name == self.1);
        f(held.map_or(&[][..], |(_, bars)| bars))
    }

    /// One field of the last closed bar, `()` before the first closes.
    fn last(&self, field: impl FnOnce(&Bar) -> rhai::Dynamic) -> rhai::Dynamic {
        self.bars(|bars| bars.last().map_or(rhai::Dynamic::UNIT, field))
    }
}

/// A builder a verb returns; `K` keeps the four kinds apart, so each takes only its own methods.
#[derive(Clone)]
struct Builder<const K: u8>(SharedContext, usize);

const ENTRY: u8 = 0;
const ORDER: u8 = 1;
const CLOSE: u8 = 2;
const EXIT: u8 = 3;

impl Handle {
    /// Records `verb`; in `on_stop` it is refused, by its name.
    fn push(&mut self, verb: Verb) -> Result<usize, Fail> {
        let mut g = self.0.write().unwrap();
        if g.reads_only {
            return Err(format!(
                "strategy.{} is refused in on_stop: the strategy is stopping (a backtest is over, a \
                 live mount is coming down), so it sends no order. on_stop reads: position(), \
                 lots() and the rest",
                verb.name()
            )
            .into());
        }
        g.calls.push(Call { verb, opts: Vec::new() });
        Ok(g.calls.len() - 1)
    }

    fn verb<const K: u8>(&mut self, verb: Verb) -> Result<Builder<K>, Fail> {
        let at = self.push(verb)?;
        Ok(Builder(self.0.clone(), at))
    }

    fn read<T>(&self, f: impl FnOnce(&Reads) -> T) -> T {
        f(&self.0.read().unwrap().reads)
    }

    /// `strategy.forecast(..)`: the forecast judged now, by the one rule, so a bad term is the hook's
    /// error at its line (and the hook then sends nothing); recorded like any verb, and refused in
    /// `on_stop` with them.
    fn forecast(
        &mut self,
        value: f64,
        horizon_ms: i64,
        confidence: Option<f64>,
    ) -> Result<(), Fail> {
        let forecast = vike_model::Forecast::new(value, horizon_ms, confidence)
            .map_err(|why| format!("strategy.forecast: {why}"))?;
        self.push(Verb::Forecast(forecast)).map(|_| ())
    }

    /// `strategy.on(symbol)`: makes `symbol` current for the verbs and reads after it.
    fn on(&mut self, symbol: &str) -> Result<Handle, Fail> {
        let mut g = self.0.write().unwrap();
        g.reads.at = g.reads.place_of(symbol).ok_or_else(|| undeclared("on", symbol))?;
        g.calls.push(Call { verb: Verb::On(symbol.into()), opts: Vec::new() });
        Ok(self.clone())
    }

    /// `f` of the reads of `symbol`, read by `strategy.<verb>(symbol)`.
    fn read_of<T>(&self, verb: &str, symbol: &str, f: impl FnOnce(&Held) -> T) -> Result<T, Fail> {
        self.read(|r| r.place_of(symbol).map(|at| f(&r.symbols[at].1)))
            .ok_or_else(|| undeclared(verb, symbol))
    }
}

/// Records `opt` on the builder's verb, which must still be the last one recorded.
fn chain<const K: u8>(b: Builder<K>, opt: Opt) -> Result<Builder<K>, Fail> {
    {
        let mut g = b.0.write().unwrap();
        if b.1 + 1 != g.calls.len() {
            return Err("an order's options chain onto the verb that made it, before the next \
                        verb: strategy.exit(\"L1\").stop(level)"
                .into());
        }
        g.calls[b.1].opts.push(opt);
    }
    Ok(b)
}

/// `#{ after_ticks: .., by_ticks: .. }`, both numbers and nothing else.
fn trail_of(map: &rhai::Map) -> Result<TacticsTrail, Fail> {
    let number = |k: &str| map.get(k).and_then(arg);
    match (number("after_ticks"), number("by_ticks")) {
        (Some(after_ticks), Some(by_ticks)) if map.len() == 2 => {
            Ok(TacticsTrail { after_ticks, by_ticks })
        }
        _ => Err("trail takes #{ after_ticks: <number>, by_ticks: <number> }".into()),
    }
}

/// A script's `fn defaults()` map: `size`, `pyramiding` and its slippage (the owner's ruling of
/// 2026-11-18: `slippage_percent`, as a rule states a percent, or `slippage_ticks` with `slippage_tick`,
/// `vike_model::Slippage`), each optional, nothing else.
pub(crate) fn defaults_of(map: &rhai::Map) -> Result<RuleDefaults, String> {
    let mut defaults = RuleDefaults::PLAIN;
    let (mut percent, mut ticks, mut tick) = (None, None, None);
    for (key, value) in map {
        let n = arg(value).ok_or_else(|| format!("defaults: `{key}` must be a number"))?;
        match key.as_str() {
            "size" => defaults.size = n,
            "pyramiding" if n >= 0.0 && n.fract() == 0.0 => defaults.pyramiding = n as usize,
            "pyramiding" => {
                return Err("defaults: `pyramiding` is a whole number of entries".into());
            }
            "slippage_percent" => percent = Some(n),
            "slippage_ticks" => ticks = Some(n),
            "slippage_tick" => tick = Some(n),
            other => {
                return Err(format!(
                    "defaults: `{other}` is not one of its fields (size, pyramiding, \
                     slippage_percent, slippage_ticks, slippage_tick)"
                ));
            }
        }
    }
    defaults.slippage = match (percent, ticks, tick) {
        (None, None, None) => Slippage::None,
        (Some(p), None, None) => Slippage::percent(p),
        (None, Some(ticks), Some(tick)) => Slippage::ticks(ticks, tick),
        _ => {
            return Err("defaults: a slippage is `slippage_percent`, or `slippage_ticks` with \
                        `slippage_tick` (the price one tick is), one of the two"
                .into());
        }
    };
    Ok(defaults)
}

/// A script's `fn inputs()` array: the names of the inputs it reads (`vike_tactics::Rule::inputs`),
/// every entry a string.
pub(crate) fn inputs_of(names: rhai::Array) -> Result<Vec<String>, String> {
    strings_of(
        names,
        "inputs",
        "the names of the inputs the script reads, [\"1d\", \"binance:ETHUSDT:1h\"]",
    )
}

/// A script's `fn trades()` array: the symbols it trades beside its own (`vike_tactics::RuleRun::trades`),
/// every entry a string.
pub(crate) fn trades_of(names: rhai::Array) -> Result<Vec<String>, String> {
    strings_of(names, "trades", "the symbols the script trades beside its own, [\"ETHUSDT\"]")
}

/// `names`, every entry a string, else `fn <hook>()`'s error saying it returns `what`.
fn strings_of(names: rhai::Array, hook: &str, what: &str) -> Result<Vec<String>, String> {
    let name = |(at, v): (usize, rhai::Dynamic)| {
        v.into_string().map_err(|got| {
            format!("{hook}: entry {} is {got}, not a string: fn {hook}() returns {what}", at + 1)
        })
    };
    names.into_iter().enumerate().map(name).collect()
}

/// Brings `reads` up to `strategy` as one hook starts, on the run's own symbol: each symbol's reads
/// (moving `strategy` onto each declared one, then back), the account's and the inputs'.
pub(crate) fn snapshot(reads: &mut Reads, strategy: &mut Tactics<'_>) {
    reads.at = 0;
    let n = reads.symbols.len();
    for (at, (symbol, held)) in reads.symbols.iter_mut().enumerate() {
        if at == 0 && symbol.as_str() != strategy.symbol() {
            *symbol = strategy.symbol().to_string();
        } else if at > 0 {
            strategy.on(symbol);
        }
        held.position = strategy.position();
        held.avg_cost = strategy.avg_cost();
        held.price = strategy.price();
        held.tick = strategy.tick();
        held.lots = strategy.lots().to_vec();
    }
    if n > 1 {
        strategy.on("");
    }
    reads.equity = strategy.equity();
    for (name, held) in &mut reads.inputs {
        follow(held, strategy.input(name).bars(usize::MAX));
    }
    match strategy.closed_trades() {
        None => reads.closed = None,
        Some(rows) => {
            // an empty ledger's net is `Tactics::closed_net`'s start, 0.0
            let (held, net) = reads.closed.get_or_insert_with(|| (Vec::new(), 0.0));
            if follow(held, rows) {
                *net = strategy.closed_net().unwrap_or_default();
            }
        }
    }
}

/// Brings `held` up to `now`, a history that grows only at its end: appends what is new, so a run
/// copies each bar or row once rather than its whole history per hook, or copies `now` whole when
/// it is not `held` extended. Answers whether `held` changed.
fn follow<T: Clone + PartialEq>(held: &mut Vec<T>, now: &[T]) -> bool {
    let n = held.len();
    if now.len() >= n && held.last() == n.checked_sub(1).map(|i| &now[i]) {
        held.extend_from_slice(&now[n..]);
        return now.len() > n;
    }
    held.clear();
    held.extend_from_slice(now);
    true
}

/// Issues what a hook recorded on the real `Tactics`, verb by verb, each with its options.
pub(crate) fn replay(strategy: &mut Tactics<'_>, calls: Vec<Call>) {
    const OWN: &str = "a builder records only its own kind's options";
    for Call { verb, opts } in calls {
        match verb {
            Verb::Entry(name, side) => {
                let e = strategy.entry(&name, side);
                for o in opts {
                    match o {
                        Opt::Size(v) => e.size(v),
                        Opt::Stop(v) => e.stop(v),
                        Opt::Limit(v) => e.limit(v),
                        Opt::Chase => e.chase(),
                        Opt::Iceberg(v) => e.iceberg(v),
                        Opt::Group(g) => e.group(&g),
                        _ => unreachable!("{OWN}"),
                    };
                }
            }
            Verb::Order(name, side) => {
                let o = strategy.order(&name, side);
                for x in opts {
                    match x {
                        Opt::Size(v) => o.size(v),
                        Opt::Stop(v) => o.stop(v),
                        Opt::Limit(v) => o.limit(v),
                        Opt::Chase => o.chase(),
                        Opt::Iceberg(v) => o.iceberg(v),
                        Opt::Group(g) => o.group(&g),
                        _ => unreachable!("{OWN}"),
                    };
                }
            }
            Verb::Close(name) => {
                let c = match &name {
                    Some(n) => strategy.close(n),
                    None => strategy.close_all(),
                };
                for o in opts {
                    match o {
                        Opt::Size(v) => c.size(v),
                        Opt::Here => c.here(),
                        _ => unreachable!("{OWN}"),
                    };
                }
            }
            Verb::Exit(name) => {
                let x = strategy.exit(&name);
                for o in opts {
                    match o {
                        Opt::From(l) => x.from(&l),
                        Opt::Size(v) => x.size(v),
                        Opt::Stop(v) => x.stop(v),
                        Opt::Take(v) => x.take(v),
                        Opt::StopPercent(v) => x.stop_pct(v),
                        Opt::TakePercent(v) => x.take_pct(v),
                        Opt::Trail(t) => x.trail(t),
                        Opt::Group(g) => x.group(&g),
                        _ => unreachable!("{OWN}"),
                    };
                }
            }
            Verb::Cancel(name) => strategy.cancel(&name),
            Verb::CancelAll => strategy.cancel_all(),
            Verb::On(symbol) => {
                strategy.on(&symbol);
            }
            Verb::Forecast(forecast) => strategy.forecast(forecast),
        }
    }
}

fn lot_map(l: &TacticsBookLot) -> rhai::Dynamic {
    let mut m = rhai::Map::new();
    m.insert("name".into(), l.name.clone().into());
    m.insert("side".into(), rhai::Dynamic::from(l.side));
    m.insert("qty".into(), l.qty.into());
    m.insert("price".into(), l.price.into());
    m.insert("entry_timestamp".into(), l.entry_timestamp.into());
    rhai::Dynamic::from_map(m)
}

/// An input's bar, keyed as the `Input` reads are named.
fn bar_map(b: &Bar) -> rhai::Dynamic {
    let mut m = rhai::Map::new();
    m.insert("time".into(), b.timestamp.into());
    m.insert("open".into(), b.open.into());
    m.insert("high".into(), b.high.into());
    m.insert("low".into(), b.low.into());
    m.insert("close".into(), b.close.into());
    m.insert("volume".into(), b.volume.into());
    debug_assert_eq!(m.len(), BAR_FIELDS, "MAX_BARS counts these fields");
    rhai::Dynamic::from_map(m)
}

/// A closed-trade row, keyed by `TradeClosedRow`'s own field names.
fn trade_map(r: &TradeClosedRow) -> rhai::Dynamic {
    let mut m = rhai::Map::new();
    m.insert("symbol".into(), r.symbol.clone().into());
    m.insert("tag".into(), r.tag.clone().into());
    let side = if r.side > 0 { OrderSide::Long } else { OrderSide::Short };
    m.insert("side".into(), rhai::Dynamic::from(side));
    m.insert("qty".into(), r.qty.into());
    m.insert("entry_timestamp".into(), r.entry_timestamp.into());
    m.insert("entry_bar".into(), (r.entry_bar as i64).into());
    m.insert("entry_price".into(), r.entry_price.into());
    m.insert("exit_timestamp".into(), r.exit_timestamp.into());
    m.insert("exit_bar".into(), (r.exit_bar as i64).into());
    m.insert("exit_price".into(), r.exit_price.into());
    m.insert("pnl".into(), r.pnl.into());
    m.insert("commission".into(), r.commission.into());
    m.insert("net".into(), r.net.into());
    debug_assert_eq!(m.len(), TRADE_FIELDS, "MAX_TRADES counts these fields");
    rhai::Dynamic::from_map(m)
}

/// Registers the `strategy` object's types, `OrderSide::Long`/`OrderSide::Short`, its verbs, builders and
/// reads. Every function here takes one of this module's types first, so no indicator or global
/// host function can shadow one (`crate::HOST_FN_NAMES` lists the global names only).
pub(crate) fn register(engine: &mut rhai::Engine) {
    engine
        .register_type_with_name::<Handle>("Tactics")
        .register_type_with_name::<OrderSide>("OrderSide")
        .register_type_with_name::<Builder<ENTRY>>("Entry")
        .register_type_with_name::<Builder<ORDER>>("Order")
        .register_type_with_name::<Builder<CLOSE>>("Close")
        .register_type_with_name::<Builder<EXIT>>("Exit");
    let mut sides = rhai::Module::new();
    sides.set_var("Long", OrderSide::Long).set_var("Short", OrderSide::Short);
    engine.register_static_module("OrderSide", sides.into());
    engine.register_fn("==", |a: OrderSide, b: OrderSide| a == b);
    engine.register_fn("!=", |a: OrderSide, b: OrderSide| a != b);

    engine.register_fn("entry", |s: &mut Handle, name: &str, side: OrderSide| {
        s.verb::<ENTRY>(Verb::Entry(name.into(), side))
    });
    engine.register_fn("order", |s: &mut Handle, name: &str, side: OrderSide| {
        s.verb::<ORDER>(Verb::Order(name.into(), side))
    });
    engine.register_fn("close", |s: &mut Handle, name: &str| {
        s.verb::<CLOSE>(Verb::Close(Some(name.into())))
    });
    engine.register_fn("close_all", |s: &mut Handle| s.verb::<CLOSE>(Verb::Close(None)));
    engine
        .register_fn("exit", |s: &mut Handle, name: &str| s.verb::<EXIT>(Verb::Exit(name.into())));
    // `strategy.exit()` would otherwise reach rhai's own `exit(value)`, which ends the hook quietly
    engine.register_fn("exit", |_: &mut Handle| -> Result<(), Fail> {
        Err("strategy.exit names the lots it protects: strategy.exit(\"L1\")".into())
    });
    engine.register_fn("cancel", |s: &mut Handle, name: &str| {
        s.push(Verb::Cancel(name.into())).map(|_| ())
    });
    engine.register_fn("cancel_all", |s: &mut Handle| s.push(Verb::CancelAll).map(|_| ()));
    engine.register_fn("forecast", |s: &mut Handle, value: f64, horizon_ms: i64| {
        s.forecast(value, horizon_ms, None)
    });
    engine.register_fn(
        "forecast",
        |s: &mut Handle, value: f64, horizon_ms: i64, confidence: f64| {
            s.forecast(value, horizon_ms, Some(confidence))
        },
    );
    engine.register_fn("on", |s: &mut Handle, symbol: &str| s.on(symbol));

    engine.register_fn("size", |b: Builder<ENTRY>, v: f64| chain(b, Opt::Size(v)));
    engine.register_fn("stop", |b: Builder<ENTRY>, v: f64| chain(b, Opt::Stop(v)));
    engine.register_fn("limit", |b: Builder<ENTRY>, v: f64| chain(b, Opt::Limit(v)));
    engine.register_fn("chase", |b: Builder<ENTRY>| chain(b, Opt::Chase));
    engine.register_fn("iceberg", |b: Builder<ENTRY>, v: f64| chain(b, Opt::Iceberg(v)));
    engine.register_fn("group", |b: Builder<ENTRY>, g: &str| chain(b, Opt::Group(g.into())));
    engine.register_fn("size", |b: Builder<ORDER>, v: f64| chain(b, Opt::Size(v)));
    engine.register_fn("stop", |b: Builder<ORDER>, v: f64| chain(b, Opt::Stop(v)));
    engine.register_fn("limit", |b: Builder<ORDER>, v: f64| chain(b, Opt::Limit(v)));
    engine.register_fn("chase", |b: Builder<ORDER>| chain(b, Opt::Chase));
    engine.register_fn("iceberg", |b: Builder<ORDER>, v: f64| chain(b, Opt::Iceberg(v)));
    engine.register_fn("group", |b: Builder<ORDER>, g: &str| chain(b, Opt::Group(g.into())));
    engine.register_fn("size", |b: Builder<CLOSE>, v: f64| chain(b, Opt::Size(v)));
    engine.register_fn("here", |b: Builder<CLOSE>| chain(b, Opt::Here));
    engine.register_fn("from", |b: Builder<EXIT>, l: &str| chain(b, Opt::From(l.into())));
    engine.register_fn("size", |b: Builder<EXIT>, v: f64| chain(b, Opt::Size(v)));
    engine.register_fn("stop", |b: Builder<EXIT>, v: f64| chain(b, Opt::Stop(v)));
    engine.register_fn("take", |b: Builder<EXIT>, v: f64| chain(b, Opt::Take(v)));
    engine.register_fn("stop_pct", |b: Builder<EXIT>, v: f64| chain(b, Opt::StopPercent(v)));
    engine.register_fn("take_pct", |b: Builder<EXIT>, v: f64| chain(b, Opt::TakePercent(v)));
    engine
        .register_fn("trail", |b: Builder<EXIT>, m: rhai::Map| chain(b, Opt::Trail(trail_of(&m)?)));
    engine.register_fn("group", |b: Builder<EXIT>, g: &str| chain(b, Opt::Group(g.into())));

    engine.register_fn("position", |s: &mut Handle| s.read(|r| r.held().position));
    engine.register_fn("position_of", |s: &mut Handle, symbol: &str| {
        s.read_of("position_of", symbol, |h| h.position)
    });
    engine.register_fn("avg_cost", |s: &mut Handle| s.read(|r| r.held().avg_cost));
    engine.register_fn("equity", |s: &mut Handle| s.read(|r| r.equity));
    engine.register_fn("price", |s: &mut Handle| s.read(|r| r.held().price));
    engine.register_fn("price_of", |s: &mut Handle, symbol: &str| {
        s.read_of("price_of", symbol, |h| h.price)
    });
    engine.register_fn("lots", |s: &mut Handle| -> rhai::Array {
        s.read(|r| r.held().lots.iter().map(lot_map).collect())
    });
    engine.register_fn("snap_stop", |s: &mut Handle, side: OrderSide, px: f64| {
        s.read(|r| snap_stop(i32::from(side), px, r.held().tick))
    });
    engine.register_fn("snap_limit", |s: &mut Handle, side: OrderSide, px: f64| {
        s.read(|r| snap_limit(i32::from(side), px, r.held().tick))
    });

    engine.register_type_with_name::<InputHandle>("Input");
    engine.register_fn("input", |s: &mut Handle, name: &str| InputHandle(s.0.clone(), name.into()));
    engine.register_fn("close", |i: &mut InputHandle| i.last(|b| b.close.into()));
    engine.register_fn("open", |i: &mut InputHandle| i.last(|b| b.open.into()));
    engine.register_fn("high", |i: &mut InputHandle| i.last(|b| b.high.into()));
    engine.register_fn("low", |i: &mut InputHandle| i.last(|b| b.low.into()));
    engine.register_fn("volume", |i: &mut InputHandle| i.last(|b| b.volume.into()));
    engine.register_fn("time", |i: &mut InputHandle| i.last(|b| b.timestamp.into()));
    engine.register_fn("bars", |i: &mut InputHandle, n: i64| -> Result<rhai::Array, Fail> {
        let n = usize::try_from(n).map_err(|_| "input.bars(n) counts bars: 0 or more")?;
        if n > MAX_BARS {
            return Err(format!(
                "input.bars({n}): one array of a script holds at most {MAX_BARS} bars (each a map of \
                 {BAR_FIELDS} fields): ask for {MAX_BARS} or fewer, or walk them one at a time with \
                 input.count() and input.bar(i)"
            )
            .into());
        }
        Ok(i.bars(|bars| bars[bars.len().saturating_sub(n)..].iter().map(bar_map).collect()))
    });
    engine.register_fn("since", |i: &mut InputHandle, ts: i64| -> Result<rhai::Array, Fail> {
        i.bars(|bars| {
            let since = &bars[bars.partition_point(|b| b.timestamp < ts)..];
            if since.len() > MAX_BARS {
                return Err(format!(
                    "input.since({ts}) holds {} bars, more than the {MAX_BARS} one array of a script \
                     holds: walk them one at a time with input.count() and input.bar(i), or read since a \
                     later time",
                    since.len()
                )
                .into());
            }
            Ok(since.iter().map(bar_map).collect())
        })
    });

    engine.register_fn("closed_trades", |s: &mut Handle| -> Result<rhai::Dynamic, Fail> {
        s.read(|r| match &r.closed {
            Some((rows, _)) if rows.len() > MAX_TRADES => Err(format!(
                "strategy.closed_trades() holds {} trades, more than the {MAX_TRADES} one array of a \
                 script holds (each a map of {TRADE_FIELDS} fields): walk them one at a time with \
                 strategy.closed_count() and strategy.closed_trade(i), or read strategy.closed_net() \
                 for their net",
                rows.len()
            )
            .into()),
            Some((rows, _)) => Ok(rhai::Dynamic::from_array(rows.iter().map(trade_map).collect())),
            None => Ok(rhai::Dynamic::UNIT),
        })
    });
    engine.register_fn("closed_net", |s: &mut Handle| {
        s.read(|r| r.closed.as_ref().map_or(rhai::Dynamic::UNIT, |(_, net)| (*net).into()))
    });

    // The index reads: one map an answer, so no array is ever built and any length can be walked.
    engine.register_fn("count", |i: &mut InputHandle| i.bars(|bars| bars.len() as i64));
    engine.register_fn("bar", |i: &mut InputHandle, at: i64| {
        i.bars(|bars| index(bars.len(), at).map_or(rhai::Dynamic::UNIT, |k| bar_map(&bars[k])))
    });
    engine.register_fn("closed_count", |s: &mut Handle| {
        s.read(|r| {
            r.closed.as_ref().map_or(rhai::Dynamic::UNIT, |(rows, _)| (rows.len() as i64).into())
        })
    });
    engine.register_fn("closed_trade", |s: &mut Handle, at: i64| {
        s.read(|r| match &r.closed {
            Some((rows, _)) => {
                index(rows.len(), at).map_or(rhai::Dynamic::UNIT, |k| trade_map(&rows[k]))
            }
            None => rhai::Dynamic::UNIT,
        })
    });
}

/// The place of `at` in a list of `len`, OLDEST FIRST: `0` the oldest, `len - 1` the newest, and a
/// negative `at` counted back from the newest (`-1` the newest), as a Rhai array's index reads. `None`
/// outside the list, which the reads answer as `()`.
fn index(len: usize, at: i64) -> Option<usize> {
    let k = if at < 0 { i64::try_from(len).ok()? + at } else { at };
    usize::try_from(k).ok().filter(|k| *k < len)
}

#[path = "tactics_tests.rs"]
#[cfg(test)]
mod tactics_tests;
