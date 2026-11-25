//! A reinforcement-learning ENVIRONMENT over the event simulator ([`crate::StrategyEngine`]), research
//! side: [`RlEnvironment::reset`] starts an episode and answers its first [`RlObservation`],
//! [`RlEnvironment::step`] takes one [`RlAction`] and answers the next observation, the reward and
//! whether the episode is over ([`RlStep`]). Nothing here reaches a live mount.
//!
//! It lives in the simulator because an episode IS one run and drives nothing else: no store, no
//! profile, no harness. So a research study FILE runs it inside its jail (the study template names this
//! crate, decision 0291), and a test or a research binary runs it in process.
//!
//! # The contract
//!
//! - **A decision is a strategy hook.** On bars, one per bar (`Strategy::on_bar`, after the bar's fills
//!   and marks, where any backtest strategy decides); on ticks, one per quote, trade or book event the
//!   engine delivers (`on_quote_tick`, `on_trade_tick`, `on_order_book`). An order the action sends fills
//!   by the simulator's own law: a market order on the NEXT bar's open, a limit when the tape reaches it.
//! - **Actions.** [`RlAction::TargetPosition`] holds a signed number of units: the difference between the
//!   target and the position the strategy sees plus the target orders still working goes as ONE tagged
//!   market order, and nothing goes when it is zero. [`RlAction::Quotes`] rests a bid and an ask as tagged
//!   limits: a new side is placed, a changed one amended in place, a `None` side cancelled and an unchanged
//!   one left alone, so a partly filled quote keeps resting its remainder. The two are independent: a
//!   target does not pull the quotes, and a quote's fill moves the position the next target reconciles.
//! - **Reward** is the change of the account's equity (`OrderBroker::equity`: cash plus marked positions,
//!   net of every fee, slippage and funding charge the engine booked) since the previous decision. The
//!   last step reaches the run's `final_equity`, so an episode's rewards sum to its final equity minus the
//!   first observation's.
//! - **Episode.** `reset(seed, window)` runs `window` consecutive bars (or tick events) of the held
//!   history, starting at the seed's FNV-1a digest (`vike_utility::fnv`) modulo the number of possible
//!   starts ([`RlEnvironment::seeded_start`]; [`RlEnvironment::reset_at`] takes the start itself, so an
//!   evaluation can walk the history window by window); a window at least as long as the history is
//!   all of it. The same seed, window and actions
//!   give the same episode bit for bit: the simulator is deterministic, and the two threads below run in
//!   lock-step.
//! - **Training and evaluation are split by TIME** ([`RlEnvironment::with_training_end`]): `reset` then
//!   draws only episodes that end before the split, and `reset_at` serves only episodes that start at
//!   least their own window after it, so an agent is never scored on bars it trained on or on the
//!   episode-length embargo just after them. Without a split, both draw from the whole history.
//! - **Costs and realism are the run's.** The caller's `StrategyEngineConfig` factory states the fees,
//!   slippage, cash and every opt-in model exactly as a backtest's engine table does (the queue model and
//!   the latency model act on the tick lane only, as in any backtest); each episode builds a fresh config.
//!   Under a latency model the observation's position is the strategy's delayed view and its equity the
//!   exchange's truth, as [`StrategyEngineConfig::latency_model`] documents.
//! - When an episode ends, [`RlEnvironment::episode_result`] holds its whole `TradingRecord` (trades,
//!   equity curve, fees) for the usual metrics.
//!
//! # Why a thread
//!
//! The engine owns its loop (`StrategyEngine::run`, `StrategyEngine::run_ticks`) and calls the strategy,
//! while an environment is called BY the agent. Each episode therefore runs the unmodified engine on its
//! own named thread, with a strategy that hands every decision across a channel and blocks until the
//! action comes back: no engine code changed, and every engine law (the order ends, the in-flight queue,
//! the emulated book) holds as in any backtest. The cost is two channel hops per decision. A reset or a
//! drop in the middle of an episode hangs up: the old run finishes its window with no agent and is joined.
//!
//! # What this door does not do
//!
//! It builds no `StrategyEngineConfig` from symbols, so the harness's profile doors' guards
//! (`vike_backtest::run::refuse_foreign_quoted_symbols`, `refuse_inverse_contracts`) are the
//! caller's when its history comes from a profile: the config it hands states the ledger and the contract
//! values, as a direct `StrategyEngine::new` caller's does. The agent's strategy declares no input
//! (decision 0271). There are no Python bindings and no gym dependency.
//!
//! shortcut: ONE traded symbol and no other series beside it; a cross-section agent needs one action per
//! symbol, add it when a chapter needs that.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use crate::{OrderBrokerSimulator, StrategyEngine, StrategyEngineConfig, TickEvent};
use vike_analytics::TradingRecord;
use vike_marketdata::{Bar, L2Book, TickQuote, TickTrade};
use vike_model::{Fill, OrderBroker, OrderEvent, OrderPlacement, Strategy};

/// What the agent does at one decision.
#[derive(Debug, Clone, PartialEq)]
pub enum RlAction {
    /// Hold this many units of the symbol (signed: negative is short), reached by one market order for
    /// the difference to what is held and already on its way.
    TargetPosition(f64),
    /// Rest these quotes as limits; `None` pulls that side.
    Quotes { bid: Option<RlQuote>, ask: Option<RlQuote> },
}

/// One resting quote: a limit at `price` for `size` units.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RlQuote {
    pub price: f64,
    pub size: f64,
}

/// The market event a decision follows: what the strategy hook was handed.
#[derive(Debug, Clone)]
pub enum RlEvent {
    Bar(Bar),
    Quote(TickQuote),
    Trade(TickTrade),
    Book(L2Book),
}

/// What the agent sees at a decision, read through the strategy's own broker.
#[derive(Debug, Clone)]
pub struct RlObservation {
    /// The engine's clock, epoch milliseconds.
    pub timestamp: i64,
    /// The event this decision follows (the last one again on the terminal observation).
    pub event: RlEvent,
    /// The symbol's mark (`OrderBroker::price`).
    pub price: f64,
    /// The signed position in units, as the strategy sees it.
    pub position: f64,
    /// The position's average entry price, `0.0` when flat.
    pub average_price: f64,
    /// The account's equity.
    pub equity: f64,
}

/// What one [`RlEnvironment::step`] answers.
#[derive(Debug, Clone)]
pub struct RlStep {
    /// The next decision's observation, or the terminal one when `done`.
    pub observation: RlObservation,
    /// The equity gained since the previous decision.
    pub reward: f64,
    /// The episode's window is over: [`RlEnvironment::reset`] starts the next one.
    pub done: bool,
}

/// The environment: a held history of ONE symbol and the engine config each episode runs with.
pub struct RlEnvironment {
    symbol: String,
    history: History,
    config: Arc<dyn Fn() -> StrategyEngineConfig + Send + Sync>,
    episode: Option<Episode>,
    result: Option<TradingRecord>,
    /// Where training ends ([`RlEnvironment::with_training_end`]); `None`: no split.
    training_end: Option<usize>,
}

/// The held tape: bars for the bar lane, tick events for the tick lane.
enum History {
    Bars(Vec<Bar>),
    Ticks(Vec<TickEvent>),
}

impl History {
    fn len(&self) -> usize {
        match self {
            History::Bars(bars) => bars.len(),
            History::Ticks(ticks) => ticks.len(),
        }
    }

    fn window(&self, start: usize, length: usize) -> History {
        match self {
            History::Bars(bars) => History::Bars(bars[start..start + length].to_vec()),
            History::Ticks(ticks) => History::Ticks(ticks[start..start + length].to_vec()),
        }
    }
}

impl RlEnvironment {
    /// An environment deciding once per bar of `bars`, the series of `symbol`.
    pub fn over_bars(
        symbol: &str,
        bars: Vec<Bar>,
        config: impl Fn() -> StrategyEngineConfig + Send + Sync + 'static,
    ) -> Self {
        Self::over(symbol, History::Bars(bars), Arc::new(config))
    }

    /// An environment deciding once per tick event of `ticks`, every one naming `symbol`.
    pub fn over_ticks(
        symbol: &str,
        ticks: Vec<TickEvent>,
        config: impl Fn() -> StrategyEngineConfig + Send + Sync + 'static,
    ) -> Self {
        Self::over(symbol, History::Ticks(ticks), Arc::new(config))
    }

    fn over(
        symbol: &str,
        history: History,
        config: Arc<dyn Fn() -> StrategyEngineConfig + Send + Sync>,
    ) -> Self {
        RlEnvironment {
            symbol: symbol.to_string(),
            history,
            config,
            episode: None,
            result: None,
            training_end: None,
        }
    }

    /// Split the held history by TIME at `end`, a bar (or tick event) index: [`RlEnvironment::reset`] then
    /// draws only training episodes that end before `end`, and [`RlEnvironment::reset_at`] serves only
    /// evaluation episodes that start at `end` plus their own window or later, an embargo of one episode.
    pub fn with_training_end(mut self, end: usize) -> Self {
        self.training_end = Some(end.min(self.history.len()));
        self
    }

    /// Start an episode of `window` bars (or tick events) at the start `seed` picks, ending any episode
    /// still running, and answer its first observation. Refused when the window holds no decision (an
    /// empty history, or a window inside the config's warm-up). With a split
    /// ([`RlEnvironment::with_training_end`]) the episode lies wholly before it, a window longer than the
    /// training part being all of that part.
    pub fn reset(&mut self, seed: u64, window: usize) -> Result<RlObservation, String> {
        let window = window.min(self.training_events());
        self.run_window(self.seeded_start(seed, window), window)
    }

    /// The first bar (or tick event) of the window [`RlEnvironment::reset`] runs for `seed`: the seed's
    /// FNV-1a digest modulo the number of possible starts (before the split, when there is one).
    pub fn seeded_start(&self, seed: u64, window: usize) -> usize {
        let held = self.training_events();
        let starts = (held - window.min(held) + 1) as u64;
        (vike_utility::fnv::fnv1a64(&seed.to_le_bytes()) % starts) as usize
    }

    /// [`RlEnvironment::reset`] at a chosen `start`, so a caller can walk a history window by window;
    /// a window running past the history's end is refused, and with a split
    /// ([`RlEnvironment::with_training_end`]) so is one starting before the split plus its window.
    pub fn reset_at(&mut self, start: usize, window: usize) -> Result<RlObservation, String> {
        let window = window.min(self.history.len());
        if let Some(end) = self.training_end
            && start < end + window
        {
            return Err(format!(
                "an evaluation window of {window} starts at {} or later, one window after the training \
                 that ends at {end}: from {start} it would hold bars trained on or within an episode of \
                 them",
                end + window
            ));
        }
        self.run_window(start, window)
    }

    /// The events training draws from: those before the split, else the whole history.
    fn training_events(&self) -> usize {
        self.training_end.unwrap_or(self.history.len())
    }

    /// Run `window` events from `start`, ending any episode still running, and answer its first
    /// observation.
    fn run_window(&mut self, start: usize, window: usize) -> Result<RlObservation, String> {
        self.episode = None;
        self.result = None;
        let held = self.history.len();
        let window = window.min(held);
        if start + window > held {
            return Err(format!(
                "a window of {window} from {start} runs past the history's {held} events"
            ));
        }
        let mut episode = self.start(self.history.window(start, window))?;
        match episode.receive() {
            Some(Decision::Next(observation)) => {
                episode.equity = observation.equity;
                self.episode = Some(episode);
                Ok(observation)
            }
            _ => Err(episode.finish().err().unwrap_or_else(|| {
                format!(
                    "the window of {window} from {start} holds no decision: it is empty, or inside \
                     the warm-up"
                )
            })),
        }
    }

    /// Act on the current decision and run the simulator to the next one.
    pub fn step(&mut self, action: RlAction) -> Result<RlStep, String> {
        refuse_non_finite(&action)?;
        let Some(episode) = self.episode.as_mut() else {
            return Err("no episode is running: call reset first".to_string());
        };
        let previous = episode.equity;
        let answer = episode.send(action).and_then(|()| episode.receive());
        match answer {
            Some(Decision::Next(observation)) => {
                episode.equity = observation.equity;
                Ok(RlStep { reward: observation.equity - previous, observation, done: false })
            }
            Some(Decision::End(observation)) => {
                if let Some(ended) = self.episode.take() {
                    self.result = Some(ended.finish()?);
                }
                Ok(RlStep { reward: observation.equity - previous, observation, done: true })
            }
            None => Err(match self.episode.take().map(Episode::finish) {
                Some(Err(why)) => why,
                _ => "the episode's simulator thread stopped before its run ended".to_string(),
            }),
        }
    }

    /// The whole backtest result of the episode that just ended; `None` while one runs or none ran.
    pub fn episode_result(&self) -> Option<&TradingRecord> {
        self.result.as_ref()
    }

    /// Run the unmodified engine over `window` on its own thread, the agent's seat as its strategy.
    fn start(&self, window: History) -> Result<Episode, String> {
        let (action_sender, action_receiver) = channel();
        let (decision_sender, decision_receiver) = channel();
        let symbol = self.symbol.clone();
        let config = Arc::clone(&self.config);
        let worker = std::thread::Builder::new()
            .name("rl-environment".to_string())
            .spawn(move || {
                let seat = Seat::new(symbol.clone(), decision_sender, action_receiver);
                match window {
                    History::Bars(bars) => {
                        StrategyEngine::new(vec![(symbol, bars)], seat, config()).run()
                    }
                    History::Ticks(ticks) => {
                        StrategyEngine::new(vec![(symbol.clone(), Vec::new())], seat, config())
                            .run_ticks(&[(symbol, ticks)])
                    }
                }
            })
            .map_err(|e| format!("the episode's simulator thread did not start: {e}"))?;
        Ok(Episode {
            link: Some((action_sender, decision_receiver)),
            worker: Some(worker),
            equity: 0.0,
        })
    }
}

/// A NaN or infinite number would poison the seat's books, so it never crosses to the engine.
fn refuse_non_finite(action: &RlAction) -> Result<(), String> {
    let numbers: Vec<f64> = match action {
        RlAction::TargetPosition(target) => vec![*target],
        RlAction::Quotes { bid, ask } => {
            [bid, ask].into_iter().flatten().flat_map(|q| [q.price, q.size]).collect()
        }
    };
    match numbers.iter().find(|n| !n.is_finite()) {
        Some(n) => Err(format!("an action's numbers must be finite, got {n} in {action:?}")),
        None => Ok(()),
    }
}

/// What the seat hands the environment.
enum Decision {
    /// A decision is due: act on this observation.
    Next(RlObservation),
    /// The run is over: the account as it ended.
    End(RlObservation),
}

/// One running episode: the two channel ends and the engine thread.
struct Episode {
    link: Option<(Sender<RlAction>, Receiver<Decision>)>,
    worker: Option<JoinHandle<TradingRecord>>,
    /// The equity at the last decision, which the next reward is measured from.
    equity: f64,
}

impl Episode {
    fn send(&self, action: RlAction) -> Option<()> {
        self.link.as_ref()?.0.send(action).ok()
    }

    fn receive(&self) -> Option<Decision> {
        self.link.as_ref()?.1.recv().ok()
    }

    /// Hang up and join the engine thread: its run's result, or why it has none. A panic's own
    /// words are already on stderr (and in the log), printed by the thread's panic hook.
    fn finish(mut self) -> Result<TradingRecord, String> {
        self.link = None;
        match self.worker.take().map(JoinHandle::join) {
            Some(Ok(result)) => Ok(result),
            _ => {
                Err("the episode's simulator thread `rl-environment` panicked; its message is on \
                      stderr and in the log"
                    .to_string())
            }
        }
    }
}

impl Drop for Episode {
    fn drop(&mut self) {
        // Hang up first, so a seat blocked on the next action returns and the run can end.
        self.link = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The strategy the agent sits in: it sends each decision to the environment, waits for the action,
/// and turns it into orders through the order seam.
struct Seat {
    symbol: String,
    decisions: Sender<Decision>,
    actions: Receiver<RlAction>,
    /// The environment hung up: decide nothing more, let the run finish.
    hung_up: bool,
    last_event: Option<RlEvent>,
    /// Each target order still working, by tag: its signed size not yet heard filled.
    targets: BTreeMap<String, f64>,
    /// The bid and the ask resting, each with its tag.
    quotes: [Option<(String, RlQuote)>; 2],
    minted: u64,
}

impl Seat {
    fn new(symbol: String, decisions: Sender<Decision>, actions: Receiver<RlAction>) -> Self {
        Seat {
            symbol,
            decisions,
            actions,
            hung_up: false,
            last_event: None,
            targets: BTreeMap::new(),
            quotes: [None, None],
            minted: 0,
        }
    }

    fn observe(&self, broker: &OrderBrokerSimulator, event: RlEvent) -> RlObservation {
        RlObservation {
            timestamp: broker.now(),
            event,
            price: broker.price(&self.symbol),
            position: broker.position(&self.symbol),
            average_price: broker.average_price(&self.symbol),
            equity: broker.equity(),
        }
    }

    fn decide(&mut self, broker: &mut OrderBrokerSimulator, event: RlEvent) {
        if self.hung_up {
            return;
        }
        let observation = self.observe(broker, event.clone());
        self.last_event = Some(event);
        if self.decisions.send(Decision::Next(observation)).is_err() {
            self.hung_up = true;
            return;
        }
        match self.actions.recv() {
            Ok(action) => self.act(broker, action),
            Err(_) => self.hung_up = true,
        }
    }

    /// A fresh tag, never reused, so a late end of an old order cannot be read as a new one's.
    fn mint(&mut self, name: &str) -> String {
        self.minted += 1;
        format!("{name}-{}", self.minted)
    }

    fn act(&mut self, broker: &mut OrderBrokerSimulator, action: RlAction) {
        match action {
            RlAction::TargetPosition(target) => {
                let working: f64 = self.targets.values().sum();
                let difference = target - broker.position(&self.symbol) - working;
                if difference != 0.0 {
                    let tag = self.mint("target");
                    let side = if difference > 0.0 { 1 } else { -1 };
                    let size = difference.abs();
                    broker.place(&self.symbol, &tag, side, size, OrderPlacement::Market, false);
                    self.targets.insert(tag, difference);
                }
            }
            RlAction::Quotes { bid, ask } => {
                self.quote(broker, 0, bid);
                self.quote(broker, 1, ask);
            }
        }
    }

    /// Bring one side (`0` the bid, `1` the ask) to `wanted`.
    fn quote(&mut self, broker: &mut OrderBrokerSimulator, slot: usize, wanted: Option<RlQuote>) {
        let (side, name) = if slot == 0 { (1, "bid") } else { (-1, "ask") };
        self.quotes[slot] = match (self.quotes[slot].take(), wanted) {
            (None, None) => None,
            (Some((tag, _)), None) => {
                broker.cancel(&tag);
                None
            }
            (None, Some(quote)) => {
                let tag = self.mint(name);
                let limit = OrderPlacement::Limit { price: quote.price };
                broker.place(&self.symbol, &tag, side, quote.size, limit, false);
                Some((tag, quote))
            }
            (Some((tag, resting)), Some(quote)) => {
                if resting != quote {
                    let size = (resting.size != quote.size).then_some(quote.size);
                    let price = (resting.price != quote.price).then_some(quote.price);
                    broker.amend(&tag, size, price, None);
                }
                Some((tag, quote))
            }
        };
    }
}

impl Strategy<OrderBrokerSimulator> for Seat {
    fn on_bar(&mut self, broker: &mut OrderBrokerSimulator, bar: &Bar) {
        self.decide(broker, RlEvent::Bar(bar.clone()));
    }

    fn on_quote_tick(&mut self, broker: &mut OrderBrokerSimulator, quote: &TickQuote) {
        self.decide(broker, RlEvent::Quote(quote.clone()));
    }

    fn on_trade_tick(&mut self, broker: &mut OrderBrokerSimulator, trade: &TickTrade) {
        self.decide(broker, RlEvent::Trade(trade.clone()));
    }

    fn on_order_book(&mut self, broker: &mut OrderBrokerSimulator, book: &L2Book) {
        self.decide(broker, RlEvent::Book(book.clone()));
    }

    fn on_fill(&mut self, _broker: &mut OrderBrokerSimulator, fill: &Fill) {
        if let Some(remaining) = fill.tag.as_ref().and_then(|tag| self.targets.get_mut(tag)) {
            *remaining -= f64::from(fill.side) * fill.size;
        }
    }

    fn on_order_event(&mut self, _broker: &mut OrderBrokerSimulator, event: &OrderEvent) {
        let Some(tag) = event.tag.as_deref() else { return };
        if !event.kind.is_terminal() {
            return;
        }
        self.targets.remove(tag);
        for slot in &mut self.quotes {
            if slot.as_ref().is_some_and(|(resting, _)| resting == tag) {
                *slot = None;
            }
        }
    }

    fn on_stop(&mut self, broker: &mut OrderBrokerSimulator) {
        if self.hung_up {
            return;
        }
        if let Some(event) = self.last_event.take() {
            let _ = self.decisions.send(Decision::End(self.observe(broker, event)));
        }
    }
}

#[cfg(test)]
mod tests;
