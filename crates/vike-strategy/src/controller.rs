//! The `Controller` seam and the `ControllerHarness` that makes it a `Strategy`.
//!
//! Design: `docs/superpowers/specs/2026-07-11-position-executor.md`. The TOP half of the
//! Hummingbot-style *controller → executor* split: the [`Controller`] decides the WHAT (emit a
//! [`PositionExecutorIntent`] or not), and [`ControllerHarness`] wraps it into a plain [`Strategy`] that
//! owns the live [`PositionExecutor`]s (the HOW). RUST-NATIVE — no Python twin.
//!
//! ## Why it is a `Strategy`, not a parallel runtime
//! [`ControllerHarness<C>`] `impl<B: OrderBroker> Strategy<B>`, so it mounts through the EXISTING
//! `CoreConfig::strategy` / `extra_mounts` seam and runs UNCHANGED on both engines (live over
//! `LiveBroker`, backtest `StrategyEngine` over `OrderBrokerSimulator`). Every order an executor emits crosses
//! the SAME order seam → drain → RiskGate → client, so the harness adds NO authority and cannot
//! bypass the gate. No new serde `Event`/wire shape, no new `Command` verb.
//!
//! ## What the harness owns
//! - the [`Controller`];
//! - the ACTIVE [`PositionExecutor`]s, **ONE per `(venue, symbol)`**, in a `Vec` (few, linear scan,
//!   deterministic insertion order: no map dep, reproducible replay);
//! - a post-exit **cooldown** stamp per `(venue, symbol)`: no re-open before
//!   `last_close_ts + cooldown_ms`.
//!
//! ## The per-callback flow
//! On each `on_bar` / `on_quote_tick` / `on_trade_tick`:
//! 1. **maybe-open** — if there is NO active executor for this `(venue, symbol)` AND the cooldown has
//!    elapsed, ask [`Controller::evaluate`]; on `Some(intent)` spawn a [`PositionExecutor`] and
//!    [`start`](PositionExecutor::start) it (submits the entry through the SAME broker).
//! 2. **drive** — [`PositionExecutor::on_refresh`], then [`PositionExecutor::on_bar`] /
//!    [`PositionExecutor::on_tick`].
//! 3. **reap** — a terminal executor (`Closed`/`Failed`/`Canceled`) records its [`PositionExecutorOutcome`]
//!    (if any), stamps the cooldown and is dropped.
//!
//! `on_fill` routes by `fill.symbol`. `on_order_event` is BROADCAST to the active executors: the
//! order seam ([`OrderBroker`]) surfaces no order-client-id to route one order by (exact with one executor
//! per pair; per-order-client-id routing needs an order-client-id-returning submit). A non-reject transition is a no-op
//! inside an executor, so the broadcast is harmless.
//!
//! ## Determinism / live↔backtest parity
//! Every decision is a pure function of [`OrderBroker`] reads + delivered fills/order-events, and ALL
//! time comes from [`OrderBroker::now`] and the fill's own epoch-ms `ts` — never a wall clock — so a
//! harness produces the SAME fills and outcomes live and in backtest (the parity gate:
//! `crates/vike-simulator/tests/controller_harness.rs`).
//!
//! ## Live params
//! [`StrategyParams::PositionController`] carries a [`PositionControllerParams`] bag;
//! [`ControllerHarness::on_params_updated`] hot-swaps the harness cooldown and, via
//! [`Controller::apply_params`], the controller's tunables ATOMICALLY (single core thread, between
//! ticks) and bumps [`ControllerHarness::params_epoch`], over the existing `Command::UpdateParams`
//! → `drive_strategy_params` plumbing (journaled). A foreign variant (e.g.
//! [`StrategyParams::SpreadMaker`]) is a no-op. A re-tune affects only FUTURE opens: an in-flight
//! [`PositionExecutor`] KEEPS its armed intent/barriers (never torn down or resized).
//!
//! Not here: a GUI executor-state surface, and venue-native exits (exits stay emulated: a plain
//! opposite-side market on the barrier hit).

use vike_marketdata::{Bar, TickQuote, TickTrade};
use vike_model::{
    Fill, OrderBroker, OrderEvent, PositionControllerParams, Strategy, StrategyParams,
};

use crate::position_executor::{PositionExecutor, PositionExecutorIntent, PositionExecutorOutcome};

/// The signal → intent brain (the WHAT): the [`ControllerHarness`] asks it whether to OPEN a
/// position for one `(venue, symbol)`; `None` = do nothing.
///
/// It reads through a SHARED `&B` ([`OrderBroker::price`] / [`OrderBroker::bars`] / [`OrderBroker::position`] /
/// [`OrderBroker::now`]), so it **structurally cannot submit orders** (the order verbs need
/// `&mut B`): a controller decides, the executor executes. It reasons in POSITIONS (a
/// [`PositionExecutorIntent`]), never in raw orders, and never sees fills. The harness calls `evaluate`
/// ONLY when it is free to open (no active executor for the pair AND the cooldown elapsed), so an
/// implementation need not re-check that.
pub trait Controller {
    /// Decide whether to open a position for `(venue, symbol)` now: `Some(intent)` opens it, `None`
    /// declines. The controller echoes `venue`/`symbol` into the [`PositionExecutorIntent`].
    fn evaluate<B: OrderBroker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionExecutorIntent>;

    /// Absorb a live-params re-tune: the [`ControllerHarness`] hands over the FULL
    /// [`PositionControllerParams`] bag (after applying its own cooldown) and the controller hot-swaps the
    /// knobs it owns, effective on its NEXT [`evaluate`](Controller::evaluate). Default: no-op.
    fn apply_params(&mut self, params: &PositionControllerParams) {
        let _ = params;
    }
}

/// One market-observation the harness drives the active executor with (the internal union of the bar
/// and tick paths).
enum StepEvent<'a> {
    /// A CLOSED bar (the `on_bar` cadence) — full OHLC barrier evaluation.
    Bar(&'a Bar),
    /// A single tick price (the `on_quote_tick` mid / `on_trade_tick` price cadence).
    Tick(f64),
}

/// The controller → executor GLUE, itself a [`Strategy`]: owns the [`Controller`], the active
/// [`PositionExecutor`]s (one per `(venue, symbol)`) and the per-pair post-exit cooldown (flow and
/// parity argument: the module doc).
///
/// Mount it like any strategy: `Box::new(ControllerHarness::new(controller, venue, cooldown_ms))` as
/// a live `Mount`, or `StrategyEngine::new(bars, ControllerHarness::new(..), params)` in
/// backtest. `C: Send` makes it a valid `Box<dyn Strategy<LiveBroker> + Send>`.
#[derive(Debug)]
pub struct ControllerHarness<C: Controller> {
    controller: C,
    /// The DEFAULT venue this harness operates under — the `venue` half of an executor key for a
    /// symbol NOT in [`venue_map`](Self::venue_map), and the value echoed into a produced intent.
    venue: String,
    /// Optional per-symbol venue routing (`symbol -> venue`): a mapped symbol's executor is keyed and
    /// [`Controller::evaluate`] asked under `venue_map[symbol]`, which makes a CROSS-VENUE controller
    /// (e.g. the shipped `funding_carry` file's `FundingCarryController`) tradeable from ONE mount.
    /// EMPTY (the default) ⇒ every symbol uses `venue`, byte-identical to the single-venue mount.
    /// Insertion order, linear scan.
    venue_map: Vec<(String, String)>,
    /// Post-exit cooldown (ms): the controller may not re-open a `(venue, symbol)` until
    /// `last_close_ts + cooldown_ms`. `0` ⇒ eligible to re-open on the very next observation.
    cooldown_ms: i64,
    /// Active executors, one per `(venue, symbol)`, in insertion order (few → linear scan, no map dep,
    /// deterministic replay).
    executors: Vec<((String, String), PositionExecutor)>,
    /// `last_close_ts` per `(venue, symbol)` — the cooldown origin, stamped when an executor is reaped.
    cooldowns: Vec<((String, String), i64)>,
    /// Terminal [`PositionExecutorOutcome`]s in completion order (a closed round-trip records one; a
    /// `Failed`/`Canceled` executor records none). The harness's inspectable result trail.
    outcomes: Vec<PositionExecutorOutcome>,
    /// Live-params generation: `0` until the first APPLIED [`StrategyParams::PositionController`]
    /// update, +1 per one; a foreign variant does NOT bump it.
    params_epoch: u64,
    /// The last [`PositionControllerParams`] bag APPLIED — the READ side ([`Strategy::params`]), retained
    /// because [`Controller::apply_params`] consumes a bag with no way back out. `None` until the
    /// first re-tune, deliberately: no `Controller` exposes its CONSTRUCTED knobs, and a fabricated
    /// bag would let a read-modify-write overwrite live knobs with invented values. A never-re-tuned
    /// mount reads as "no typed params"; the whole-object escape hatch is how an operator seeds one.
    last_params: Option<PositionControllerParams>,
}

impl<C: Controller> ControllerHarness<C> {
    /// A harness driving `controller` under `venue`, spacing successive positions per pair by
    /// `cooldown_ms` (`0` = no cooldown). No executors until the first `on_*` invitation opens one.
    pub fn new(controller: C, venue: impl Into<String>, cooldown_ms: i64) -> Self {
        ControllerHarness {
            controller,
            venue: venue.into(),
            venue_map: Vec::new(),
            cooldown_ms,
            executors: Vec::new(),
            cooldowns: Vec::new(),
            outcomes: Vec::new(),
            params_epoch: 0,
            last_params: None,
        }
    }

    /// Set the per-symbol venue routing (`symbol -> venue`), overwriting any prior map — see
    /// [`venue_map`](Self::venue_map). An unmapped symbol routes to the default `venue`.
    pub fn with_venue_map(mut self, venue_map: Vec<(String, String)>) -> Self {
        self.venue_map = venue_map;
        self
    }

    /// The venue `symbol` trades under: its [`venue_map`](Self::venue_map) entry, else the default
    /// `venue`. It keys the executor AND is the venue passed to [`Controller::evaluate`].
    fn venue_for(&self, symbol: &str) -> &str {
        venue_of(&self.venue_map, &self.venue, symbol)
    }

    /// The index of the live executor keyed `(venue, symbol)`, compared in place: a key is owned only
    /// once an executor is spawned, never built per observation.
    fn executor_of(&self, venue: &str, symbol: &str) -> Option<usize> {
        self.executors.iter().position(|((v, s), _)| v == venue && s == symbol)
    }

    /// The terminal outcomes recorded so far (one per CLOSED round-trip, completion order).
    pub fn outcomes(&self) -> &[PositionExecutorOutcome] {
        &self.outcomes
    }

    /// How many executors are currently live (mid-lifecycle).
    pub fn active_count(&self) -> usize {
        self.executors.len()
    }

    /// The controller being driven (read access — e.g. to inspect its state in a test).
    pub fn controller(&self) -> &C {
        &self.controller
    }

    /// The live-params generation: `0` before any update, +1 per APPLIED
    /// [`StrategyParams::PositionController`] re-tune (a foreign variant leaves it unchanged).
    pub fn params_epoch(&self) -> u64 {
        self.params_epoch
    }

    // ---- internals ----

    /// Step 1 — ask the controller to open a position for `symbol`, iff there is no active executor
    /// for it and its cooldown has elapsed. Spawns + starts the executor (which submits the entry).
    fn maybe_open<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str) {
        if symbol.is_empty() {
            return; // an un-tagged event can't be keyed/routed — decline (fail-safe, no open)
        }
        // Read off the two fields alone, so it stays held while the controller and the executors are borrowed.
        let venue = venue_of(&self.venue_map, &self.venue, symbol);
        if self.executor_of(venue, symbol).is_some() {
            return; // one executor per (venue, symbol) — already live
        }
        let now = broker.now();
        if !self.cooldown_ok(venue, symbol, now) {
            return; // still cooling down since the last close
        }
        if let Some(intent) = self.controller.evaluate(&*broker, venue, symbol) {
            // The broker's own unit for the symbol, so the executor's PnL is the ledger's.
            let value = broker.contract_value(symbol).unwrap_or_default();
            let mut executor = PositionExecutor::new(intent).with_contract_value(value);
            executor.start(broker); // submits the entry through the SAME broker → drain → gate
            self.executors.push(((venue.to_string(), symbol.to_string()), executor));
        }
    }

    /// Steps 2+3 — advance the executor for `symbol` (refresh/retry timer, then the barrier check for
    /// this observation), then reap it if it went terminal.
    fn drive<B: OrderBroker>(&mut self, broker: &mut B, symbol: &str, event: StepEvent) {
        let Some(index) = self.executor_of(self.venue_for(symbol), symbol) else {
            return;
        };
        {
            let executor = &mut self.executors[index].1;
            executor.on_refresh(broker); // retry a backed-off entry / reprice a resting limit
            match event {
                StepEvent::Bar(bar) => executor.on_bar(broker, bar),
                StepEvent::Tick(px) => executor.on_tick(broker, px),
            }
        }
        self.reap_terminals(broker.now());
    }

    /// Step 3 — drop every executor that has reached a terminal state, recording its outcome and
    /// stamping the pair's cooldown. The cooldown origin is the executor's `exit_timestamp` for a closed
    /// round-trip (bit-identical across runtimes) and `now` for a no-position terminal
    /// (`Failed`/`Canceled`).
    fn reap_terminals(&mut self, now: i64) {
        let mut i = 0;
        while i < self.executors.len() {
            if self.executors[i].1.is_terminal() {
                let (key, executor) = self.executors.remove(i);
                let outcome = executor.outcome();
                let close_timestamp = outcome.map(|o| o.exit_timestamp).unwrap_or(now);
                if let Some(o) = outcome {
                    self.outcomes.push(o);
                }
                self.set_cooldown(key, close_timestamp);
            } else {
                i += 1;
            }
        }
    }

    /// `true` if `(venue, symbol)` may open now: no cooldown recorded, or `now` has reached
    /// `last_close + cooldown`.
    fn cooldown_ok(&self, venue: &str, symbol: &str, now: i64) -> bool {
        match self.cooldowns.iter().find(|((v, s), _)| v == venue && s == symbol) {
            Some((_, last)) => now >= last.saturating_add(self.cooldown_ms),
            None => true,
        }
    }

    /// Record/overwrite `key`'s `last_close_ts`.
    fn set_cooldown(&mut self, key: (String, String), ts: i64) {
        if let Some(slot) = self.cooldowns.iter_mut().find(|(k, _)| *k == key) {
            slot.1 = ts;
        } else {
            self.cooldowns.push((key, ts));
        }
    }
}

impl<B: OrderBroker, C: Controller> Strategy<B> for ControllerHarness<C> {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = bar.symbol.as_deref().unwrap_or_default();
        self.maybe_open(broker, symbol);
        self.drive(broker, symbol, StepEvent::Bar(bar));
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        self.maybe_open(broker, &q.symbol);
        self.drive(broker, &q.symbol, StepEvent::Tick(q.mid()));
    }

    fn on_trade_tick(&mut self, broker: &mut B, t: &TickTrade) {
        self.maybe_open(broker, &t.symbol);
        self.drive(broker, &t.symbol, StepEvent::Tick(t.price));
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        if let Some(index) = self.executor_of(self.venue_for(&fill.symbol), &fill.symbol) {
            self.executors[index].1.on_fill(fill);
        }
        self.reap_terminals(broker.now());
    }

    fn on_order_event(&mut self, broker: &mut B, event: &OrderEvent) {
        // No order-client-id on the portable OrderBroker: broadcast (see the module doc).
        for (_key, executor) in self.executors.iter_mut() {
            executor.on_order_event(broker, event);
        }
        self.reap_terminals(broker.now());
    }

    /// Live-params re-tune: hot-swap the harness cooldown + the controller's tunables ATOMICALLY
    /// (single core thread, between ticks) and bump [`params_epoch`](Self::params_epoch), WITHOUT
    /// unmounting. The broker is UNTOUCHED and the active [`PositionExecutor`]s are NOT disturbed:
    /// only FUTURE opens see the new size/barriers. A foreign [`StrategyParams`] variant is a NO-OP
    /// (the refutable `let … else`): nothing swaps and the epoch does not move.
    fn on_params_updated(&mut self, _broker: &mut B, params: &StrategyParams) {
        let StrategyParams::PositionController(p) = params else {
            return; // not our variant — ignore (additive; SpreadMaker etc. are untouched)
        };
        self.cooldown_ms = p.cooldown_ms; // harness-level knob
        self.controller.apply_params(p); // controller-level knobs (size / barriers / threshold)
        // The READ side's only copy; stamped AFTER both applies, so a read never reports a swap
        // that did not land.
        self.last_params = Some(*p);
        self.params_epoch += 1;
    }

    /// The READ side of the live-parameter plane: the last [`PositionControllerParams`] bag applied, or
    /// `None` before the first re-tune (see `last_params`). A RETAINED bag, not a live read: it is
    /// authoritative only while every `apply_params` absorbs the bag verbatim; a controller that
    /// clamps a knob must publish its own read (as `SpreadMaker::params` does).
    fn params(&self) -> Option<StrategyParams> {
        self.last_params.map(StrategyParams::PositionController)
    }
}

/// [`ControllerHarness::venue_for`] over the two fields it reads: the `venue_map` entry of `symbol`, else
/// `venue`.
fn venue_of<'a>(venue_map: &'a [(String, String)], venue: &'a str, symbol: &str) -> &'a str {
    venue_map.iter().find(|(s, _)| s == symbol).map(|(_, v)| v.as_str()).unwrap_or(venue)
}

#[path = "controller_tests.rs"]
#[cfg(test)]
mod controller_tests;
