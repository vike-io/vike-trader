//! The pipe between a tradehub and ONE sandboxed strategy process (plan 2b §4, decision 0273's table):
//! the messages, their codec, and the two halves that must agree about a broker READ, kept in one file
//! so they cannot drift apart: [`ViewRow::of`] asks the core's own `LiveBroker` what a hook would read,
//! and [`ViewBroker`] answers the strategy file from those answers inside the child.
//!
//! - **Open.** [`ToChild::Open`] is answered by [`FromChild::Opened`] (what the core asks a strategy
//!   synchronously at mount: `symbols`, `inputs`, `warmup`) or [`FromChild::Refused`].
//! - **Events.** [`ToChild::Event`] is one hook call the core made on the proxy, with its `view`; it is
//!   answered by [`FromChild::Reply`] (the [`BrokerCall`]s the hook made, in order) with the same `seq`.
//!   ONE event is in flight per mount. A new bar is appended to the history the child already holds,
//!   never re-sent ([`SeriesDelta`]).
//! - **State.** [`FromChild::State`] carries the strategy's `save_state` and `params` when they changed,
//!   at most every [`STATE_REFRESH`] and after a re-tune, AFTER the reply it follows (never in its way):
//!   the proxy answers the core's `save_state`/`params` from that cache, so the fold never waits.
//!
//! **The codec is msgpack, not the node wire's JSON** (`vike_hub_protocol::frame` writes JSON): the pipe
//! carries NaN by design (a declared mount's `average_price` row is NaN, a buggy strategy's price is refused
//! by the drain in words), and `serde_json` writes a non-finite `f64` as `null`, which reads back as an
//! error or as `None`. Msgpack keeps every bit. The frame is the node wire's: a big-endian `u32` length,
//! then the body, under the same [`vike_hub_protocol::frame::MAX_FRAME_LEN`].
//!
//! **The intents are verb calls, replayed onto the core's own `LiveBroker`.** The child's broker records
//! each `OrderBroker` write it is asked for ([`BrokerCall`]); the core replays them through the
//! same trait onto a `LiveBroker` it builds for the mount, so the buffering code is the core's own, then
//! drains it. ⚠ [`ViewBroker`] overrides exactly the write verbs `LiveBroker` overrides (`place`,
//! `place_group`, `amend`, `cancel`, `deny`) and leaves every other default to
//! the trait, which funnels into `place` on both sides: a verb `LiveBroker` starts overriding must be
//! overridden here too, or the child would buffer what the default buffers instead.

use std::io::{self, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::{
    ContractValue, FeedStatus, Fill, FlowToxicity, Grid, OrderBroker, OrderEvent, OrderListLeg,
    OrderPlacement, StrategyParams, StreamDelay, TickMark, TradeClosedRow,
};

/// How often, at most, the child sends its strategy's `save_state` / `params` (when they changed):
/// the staleness of the proxy's cache, which a restart of the CHILD reloads (a restart of the tradehub
/// reads the sidecar the core wrote from that cache).
pub const STATE_REFRESH: Duration = Duration::from_secs(1);

/// What the tradehub sends the child.
#[derive(Debug, Serialize, Deserialize)]
pub enum ToChild {
    /// The first message, once per child.
    Open(Box<Open>),
    /// One hook call.
    Event(Box<Event>),
}

/// Open the strategy: build it from its params and hand it its durable state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Open {
    /// The strategy file, as its folder names it (for the child's own words).
    pub file: String,
    /// The file's sha256, recorded (the mount logs it).
    pub sha256: String,
    /// The mount's params as TOML TEXT, the form the C-ABI takes
    /// (`vike_rust_strategy::host::StrategyFileHost::new`).
    pub params: String,
    /// The mount FACTS (`vike_model::params::MountFacts`, the strategy file interface 9): the symbol, venue
    /// and interval it runs on, and the tick size and fee rates a maker reads; handed to the file beside
    /// the params.
    pub facts: vike_model::params::MountFacts,
    /// The state to `load_state` before the first hook: a restarted child's last cached state, `None`
    /// on a fresh mount (the core then forwards the sidecar's through [`Hook::LoadState`]).
    pub state: Option<serde_json::Value>,
}

/// One hook call the core made on the mount's proxy.
#[derive(Debug, Serialize, Deserialize)]
pub struct Event {
    pub sequence: u64,
    pub hook: Hook,
    /// The broker's read side at the call; `None` for [`Hook::LoadState`], which has no broker.
    pub view: Option<View>,
}

/// The hook and its payload: one variant per `vike_model::Strategy` hook that takes a broker, plus
/// `load_state`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Hook {
    Start,
    Stop,
    Bar(Bar),
    TickQuote(TickQuote),
    TickTrade(TickTrade),
    OrderBook(L2Book),
    Schedule(String),
    Fill(Fill),
    FeedStatus(FeedStatus),
    Mark(TickMark),
    ReferenceQuote { venue: String, quote: TickQuote },
    Flow(FlowToxicity),
    OrderEvent(OrderEvent),
    ParamsUpdated(Box<StrategyParams>),
    LoadState(serde_json::Value),
}

impl Hook {
    /// The hook's name, for the reason a missed deadline is reported in.
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Hook::Start => "on_start",
            Hook::Stop => "on_stop",
            Hook::Bar(_) => "on_bar",
            Hook::TickQuote(_) => "on_quote_tick",
            Hook::TickTrade(_) => "on_trade_tick",
            Hook::OrderBook(_) => "on_order_book",
            Hook::Schedule(_) => "on_schedule",
            Hook::Fill(_) => "on_fill",
            Hook::FeedStatus(_) => "on_feed_status",
            Hook::Mark(_) => "on_mark",
            Hook::ReferenceQuote { .. } => "on_reference_quote",
            Hook::Flow(_) => "on_flow",
            Hook::OrderEvent(_) => "on_order_event",
            Hook::ParamsUpdated(_) => "on_params_updated",
            Hook::LoadState(_) => "load_state",
        }
    }

    /// Whether the hook must reach a restarted child (an order's end, a fill, a re-tune, a state, a
    /// schedule, a feed's death): the hooks a restart re-delivers and a full queue never drops.
    #[must_use]
    pub fn is_lossless(&self) -> bool {
        matches!(
            self,
            Hook::Fill(_)
                | Hook::OrderEvent(_)
                | Hook::ParamsUpdated(_)
                | Hook::LoadState(_)
                | Hook::Schedule(_)
                | Hook::FeedStatus(_)
                | Hook::Start
                | Hook::Stop
        )
    }
}

/// The `LiveBroker` read side of one hook call, as the core answered it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct View {
    /// One row per symbol the mount can be asked about: `[0]` is the mount's own (named `""` on a mount
    /// that declared no symbol, its own symbol otherwise), then each declared symbol.
    pub rows: Vec<ViewRow>,
    /// Whether the mount declared symbols (`LiveBroker::bar_views` non-empty): an undeclared mount answers
    /// any name with its own row, a declared one answers an undeclared name with nothing.
    pub declared: bool,
    pub equity: f64,
    pub index: usize,
    pub now: i64,
    /// The closed-bar history of each row's symbol, as a change to what the child holds.
    pub bars: Vec<SeriesDelta>,
    /// The as-of bars of each declared input, as a change to what the child holds.
    pub inputs: Vec<SeriesDelta>,
    /// The closed-trade ledger from row `closed_from` on (the rows before it the child holds).
    pub closed_from: usize,
    pub closed: Vec<TradeClosedRow>,
}

/// What a hook reads about ONE symbol, as the core's `LiveBroker` answered it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ViewRow {
    pub name: String,
    pub position: f64,
    pub price: f64,
    pub average_price: f64,
    pub data_delay: Option<StreamDelay>,
    pub grid: Option<Grid>,
    pub contract_value: Option<ContractValue>,
}

impl ViewRow {
    /// The row of `name`, read through `broker`'s own answers (the core's `LiveBroker`), so the child
    /// reads exactly what an in-process hook would.
    pub fn of<B: OrderBroker>(broker: &B, name: &str) -> ViewRow {
        ViewRow {
            name: name.to_string(),
            position: OrderBroker::position(broker, name),
            price: broker.price(name),
            average_price: broker.average_price(name),
            data_delay: broker.data_delay(name),
            grid: broker.instrument_grid(name),
            contract_value: broker.contract_value(name),
        }
    }
}

/// A change to one series the child holds: drop what it holds from `from` on, then append `bars`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SeriesDelta {
    pub name: String,
    pub from: usize,
    pub bars: Vec<Bar>,
}

impl SeriesDelta {
    /// The change that brings a child holding `sent` (its length and its last bar) to `now`, or `None`
    /// when it already holds `now`. The history only grows, or re-closes its last bar in place (a
    /// reconnect's overlap), so the common case sends only the new bars; anything else resends it whole.
    #[must_use]
    pub fn between(name: &str, sent: Option<&(usize, Bar)>, now: &[Bar]) -> Option<SeriesDelta> {
        let from = match sent {
            Some((n, last)) if *n > 0 && *n <= now.len() && now[*n - 1] == *last => *n,
            Some((n, last))
                if *n > 0 && *n <= now.len() && now[*n - 1].timestamp == last.timestamp =>
            {
                *n - 1
            }
            Some((0, _)) | None if now.is_empty() => return None,
            _ => 0,
        };
        if sent.is_some_and(|(n, _)| *n == now.len()) && from == now.len() {
            return None;
        }
        Some(SeriesDelta { name: name.to_string(), from, bars: now[from..].to_vec() })
    }
}

/// One write a strategy made on the child's broker, replayed in order onto the core's own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum BrokerCall {
    Place {
        symbol: String,
        tag: String,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    },
    PlaceGroup {
        symbol: String,
        legs: Vec<(String, OrderListLeg)>,
    },
    Amend {
        tag: String,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    },
    Cancel {
        tag: String,
    },
    Deny {
        symbol: String,
        tag: String,
        qty: f64,
        reason: String,
    },
}

impl BrokerCall {
    /// The symbol the call names (`""` is the mount's own), or `None` for a call by tag.
    #[must_use]
    pub fn symbol(&self) -> Option<&str> {
        match self {
            BrokerCall::Place { symbol, .. }
            | BrokerCall::PlaceGroup { symbol, .. }
            | BrokerCall::Deny { symbol, .. } => Some(symbol),
            BrokerCall::Amend { .. } | BrokerCall::Cancel { .. } => None,
        }
    }

    /// Make this call on `broker`, through the trait the strategy made it through.
    pub fn apply<B: OrderBroker>(self, broker: &mut B) {
        match self {
            BrokerCall::Place { symbol, tag, side, qty, order, reduce_only } => {
                broker.place(&symbol, &tag, side, qty, order, reduce_only);
            }
            BrokerCall::PlaceGroup { symbol, legs } => broker.place_group(&symbol, legs),
            BrokerCall::Amend { tag, new_qty, new_price, new_trigger } => {
                broker.amend(&tag, new_qty, new_price, new_trigger);
            }
            BrokerCall::Cancel { tag } => broker.cancel(&tag),
            BrokerCall::Deny { symbol, tag, qty, reason } => {
                broker.deny(&symbol, &tag, qty, &reason)
            }
        }
    }
}

/// What the child sends the tradehub.
#[derive(Debug, Serialize, Deserialize)]
pub enum FromChild {
    Opened(Opened),
    Refused { reason: String },
    Reply { sequence: u64, calls: Vec<BrokerCall> },
    State { state: Option<serde_json::Value>, params: Option<Box<StrategyParams>> },
}

/// What the core asks a strategy synchronously at mount, answered once.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opened {
    pub symbols: Vec<String>,
    pub inputs: Vec<String>,
    pub warmup: usize,
    pub state: Option<serde_json::Value>,
    pub params: Option<Box<StrategyParams>>,
    /// The file's effective-params line (its optional `echo` export), `(key, value)` in its order.
    pub echo: Option<Vec<(String, String)>>,
}

/// Write `msg` as ONE frame: the node wire's length prefix and the msgpack body in a single write (the
/// reason is `vike_hub_protocol::frame::write_frame`'s: a split write stalls a socket on Nagle).
///
/// # Errors
/// An encoding failure or a body over the frame cap ([`io::ErrorKind::InvalidData`]); the write's own.
pub fn write_message<W: Write>(w: &mut W, msg: &impl Serialize) -> io::Result<()> {
    let mut frame = vec![0u8; 4];
    rmp_serde::encode::write_named(&mut frame, msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    let len = u32::try_from(frame.len() - 4)
        .ok()
        .filter(|l| *l <= vike_hub_protocol::frame::MAX_FRAME_LEN)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "frame over the cap"))?;
    frame[..4].copy_from_slice(&len.to_be_bytes());
    w.write_all(&frame)?;
    w.flush()
}

/// Read ONE frame and decode it.
///
/// # Errors
/// The read's own (an EOF is [`io::ErrorKind::UnexpectedEof`]), or [`io::ErrorKind::InvalidData`] for a
/// body that is not a message: garbage.
pub fn read_message<R: Read, T: serde::de::DeserializeOwned>(r: &mut R) -> io::Result<T> {
    let body = vike_hub_protocol::frame::read_frame_raw(r)?;
    rmp_serde::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// The child's broker: answers every read from the last [`View`] and records every write as a
/// [`BrokerCall`]. Its read partition is `LiveBroker`'s, carried by the rows the core read through it.
#[derive(Debug, Default)]
pub struct ViewBroker {
    rows: Vec<ViewRow>,
    declared: bool,
    equity: f64,
    index: usize,
    now: i64,
    bars: Vec<(String, Vec<Bar>)>,
    inputs: Vec<(String, Vec<Bar>)>,
    closed: Vec<TradeClosedRow>,
    /// The writes of the hook running now, taken by the serve loop after it returns.
    pub calls: Vec<BrokerCall>,
}

impl ViewBroker {
    /// Take in the view of the next hook.
    ///
    /// # Errors
    /// A change that does not fit what the child holds (the parent's bookkeeping and the child's
    /// disagree), in words: the child then stops, and the parent restarts it from nothing.
    pub fn absorb(&mut self, view: View) -> Result<(), String> {
        let View { rows, declared, equity, index, now, bars, inputs, closed_from, closed } = view;
        (self.rows, self.declared, self.equity, self.index, self.now) =
            (rows, declared, equity, index, now);
        for d in bars {
            apply_delta(&mut self.bars, d)?;
        }
        for d in inputs {
            apply_delta(&mut self.inputs, d)?;
        }
        if closed_from > self.closed.len() {
            return Err(format!("closed trades from {closed_from}, {} held", self.closed.len()));
        }
        self.closed.truncate(closed_from);
        self.closed.extend(closed);
        Ok(())
    }

    /// The row a read of `name` answers from: `""` is the mount's own, a declared name its own row, and
    /// any other name the own row on an undeclared mount, nothing on a declared one.
    fn row(&self, name: &str) -> Option<&ViewRow> {
        if name.is_empty() || !self.declared {
            return self.rows.first();
        }
        self.rows.iter().find(|r| r.name == name)
    }

    fn series<'a>(held: &'a [(String, Vec<Bar>)], name: &str) -> &'a [Bar] {
        held.iter().find(|(n, _)| n == name).map_or(&[], |(_, s)| s.as_slice())
    }
}

fn apply_delta(held: &mut Vec<(String, Vec<Bar>)>, d: SeriesDelta) -> Result<(), String> {
    let at = match held.iter().position(|(n, _)| *n == d.name) {
        Some(at) => at,
        None => {
            held.push((d.name.clone(), Vec::new()));
            held.len() - 1
        }
    };
    let series = &mut held[at].1;
    if d.from > series.len() {
        return Err(format!("series `{}` from {}, {} held", d.name, d.from, series.len()));
    }
    series.truncate(d.from);
    series.extend(d.bars);
    Ok(())
}

impl OrderBroker for ViewBroker {
    fn place(
        &mut self,
        symbol: &str,
        tag: &str,
        side: i32,
        qty: f64,
        order: OrderPlacement,
        reduce_only: bool,
    ) {
        let (symbol, tag) = (symbol.to_string(), tag.to_string());
        self.calls.push(BrokerCall::Place { symbol, tag, side, qty, order, reduce_only });
    }
    fn place_group(&mut self, symbol: &str, legs: Vec<(String, OrderListLeg)>) {
        self.calls.push(BrokerCall::PlaceGroup { symbol: symbol.to_string(), legs });
    }
    fn amend(
        &mut self,
        tag: &str,
        new_qty: Option<f64>,
        new_price: Option<f64>,
        new_trigger: Option<f64>,
    ) {
        let tag = tag.to_string();
        self.calls.push(BrokerCall::Amend { tag, new_qty, new_price, new_trigger });
    }
    fn cancel(&mut self, tag: &str) {
        self.calls.push(BrokerCall::Cancel { tag: tag.to_string() });
    }
    fn deny(&mut self, symbol: &str, tag: &str, qty: f64, reason: &str) {
        let (symbol, tag, reason) = (symbol.to_string(), tag.to_string(), reason.to_string());
        self.calls.push(BrokerCall::Deny { symbol, tag, qty, reason });
    }
    fn average_price(&self, symbol: &str) -> f64 {
        self.row(symbol).map_or(0.0, |r| r.average_price)
    }
    fn position(&self, symbol: &str) -> f64 {
        self.row(symbol).map_or(0.0, |r| r.position)
    }
    fn price(&self, symbol: &str) -> f64 {
        self.row(symbol).map_or(0.0, |r| r.price)
    }
    fn equity(&self) -> f64 {
        self.equity
    }
    fn bars(&self, symbol: &str) -> &[Bar] {
        if !self.declared {
            return self.bars.first().map_or(&[], |(_, s)| s.as_slice());
        }
        let name =
            if symbol.is_empty() { self.rows.first().map_or("", |r| &r.name) } else { symbol };
        Self::series(&self.bars, name)
    }
    fn index(&self) -> usize {
        self.index
    }
    fn now(&self) -> i64 {
        self.now
    }
    fn data_delay(&self, symbol: &str) -> Option<StreamDelay> {
        self.row(symbol).and_then(|r| r.data_delay)
    }
    fn instrument_grid(&self, symbol: &str) -> Option<Grid> {
        self.row(symbol).and_then(|r| r.grid)
    }
    fn contract_value(&self, symbol: &str) -> Option<ContractValue> {
        self.row(symbol).and_then(|r| r.contract_value)
    }
    fn input_bars(&self, name: &str) -> &[Bar] {
        Self::series(&self.inputs, name)
    }
    fn closed_trades(&self) -> Option<&[TradeClosedRow]> {
        Some(&self.closed)
    }
}
