//! position-executor STAGE 5 — the `Controller` seam + the `ControllerHarness` `Strategy` glue.
//!
//! Design: `docs/superpowers/specs/2026-07-11-position-executor.md` (§3.1 / §3.4, PR5). This is the
//! TOP half of the Hummingbot-style *controller → executor* split: the [`Controller`] decision seam
//! (the WHAT — emit a [`PositionIntent`] or not) and the [`ControllerHarness`] that WRAPS it into a
//! plain [`Strategy`] (the glue that owns the live [`PositionExecutor`]s — the HOW).
//!
//! ## Why it is a `Strategy`, not a parallel runtime (spec §3.4, Q2)
//! [`ControllerHarness<C>`] `impl<B: Broker> Strategy<B>` — it IS a strategy. So it mounts through the
//! EXISTING `CoreConfig::strategy` / `extra_mounts` seam and runs UNCHANGED on BOTH engines (the live
//! single-writer runtime over `LiveBroker`, and the backtest `StrategyEngine` over `SimBroker`) — the
//! codebase's write-once rule. Direct-submit strategies (`SpreadMaker`, every backtest reference
//! strategy) are untouched; a strategy adopts managed positions by mounting a harness instead of
//! itself. Every order an executor emits still crosses the SAME `Broker` handle → drain → RiskGate →
//! client, so the harness adds NO authority and cannot bypass the gate (spec §3.4). Additive: no serde
//! `Event`/wire change, no new `Command` verb — it is composed entirely from the existing `Strategy`
//! handlers (`on_bar`/`on_quote_tick`/`on_trade_tick`/`on_fill`/`on_order_event`) plus the stage-1..4
//! [`PositionExecutor`]. RUST-NATIVE — no Python twin (like [`crate::position_executor`]).
//!
//! ## What the harness owns (spec §3.1)
//! - the [`Controller`] (asked, per (venue, symbol), whether to OPEN a position);
//! - the ACTIVE [`PositionExecutor`]s, **ONE per `(venue, symbol)`** (the v1 default, spec Q3) — a
//!   `Vec` keyed by the pair (few, linear-scan, deterministic insertion order — the same "no map dep,
//!   reproducible replay" rationale as `MultiPaperExecutionClient`);
//! - a post-exit **cooldown** stamp per `(venue, symbol)` — the stage-4-deferred controller-level gate
//!   (`last_close_ts + cooldown_ms` before the controller may open that pair again, spec §3.2).
//!
//! ## The per-callback flow (the wrap, spec §3.1)
//! On each `on_bar` / `on_quote_tick` / `on_trade_tick`:
//! 1. **maybe-open** — if there is NO active executor for this `(venue, symbol)` AND the cooldown has
//!    elapsed, ask [`Controller::evaluate`]; on `Some(intent)` spawn a [`PositionExecutor`] and
//!    [`start`](PositionExecutor::start) it (submits the entry through the SAME broker).
//! 2. **drive** — advance the active executor: its refresh/retry timer
//!    ([`PositionExecutor::on_refresh`]) then the barrier evaluation
//!    ([`PositionExecutor::on_bar`] / [`PositionExecutor::on_tick`]).
//! 3. **reap** — when an executor reaches a terminal state (`Closed`/`Failed`/`Canceled`) record its
//!    [`ExecutorOutcome`] (if any), stamp the cooldown from its close ts, and drop it.
//!
//! `on_fill` routes the execution to the active executor for `fill.symbol` (its accounting drives the
//! `EntryWorking → Open` / `Closing → Closed` transitions). `on_order_event` forwards the reject/deny
//! the executor's retry/refresh reacts to (§3.4) — broadcast to the active executors, because the
//! portable [`Broker`] surface surfaces no client-order-id to route a single order by (with one
//! executor per pair, and typically one pair per mount, this is exact; per-coid routing needs a
//! coid-returning submit, a later enhancement). A non-reject transition (`Accepted`/`Canceled`/
//! `Expired`) is a no-op inside the executor, so the broadcast is harmless.
//!
//! ## Determinism / live↔backtest parity
//! Every decision is a pure function of the portable [`Broker`] reads + delivered fills/order-events,
//! and ALL time comes from the injected [`Broker::now`] and the fill's own epoch-ms `ts` — never a
//! wall clock. Both runtimes deliver those identically (fills fold BEFORE `on_bar` each bar; `now` is
//! the bar/fill ts), so a harness produces the SAME fills and outcomes live and in backtest — the
//! "same `Strategy` runs live AND backtest" guarantee, proven by the stage-5 backtest-parity gate
//! (`vike-backtest/tests/controller_harness.rs`).
//!
//! ## Live-params (stage 6 — this change)
//! A mounted harness is RE-TUNED live without unmounting, exactly like `SpreadMaker`: the new
//! [`StrategyParams::PositionController`] variant carries a [`ControllerParams`] bag (the harness
//! cooldown + the controller's intent template (size / triple barrier) + the reference controller's
//! decision knob); [`ControllerHarness::on_params_updated`] hot-swaps the harness cooldown and, via
//! [`Controller::apply_params`], the controller's tunables ATOMICALLY (single core thread, between
//! ticks) and bumps [`ControllerHarness::params_epoch`]. It rides the EXISTING
//! `Command::UpdateParams` → `ParamsUpdate` → `drive_strategy_params` plumbing (journaled) with ZERO
//! new lanes — no serde `Event`/wire change, no new `Command` verb. A foreign variant (e.g.
//! [`StrategyParams::SpreadMaker`]) is a no-op. Policy (spec §3.4): a re-tune affects only FUTURE
//! opens — an in-flight [`PositionExecutor`] KEEPS its armed intent/barriers (it is never torn down
//! or retroactively resized), mirroring the maker's "re-tune doesn't disturb resting orders" rule.
//!
//! ## Deferred beyond stage 6 (NOT here)
//! The GUI executor-state snapshot surface (a read-side to show live executor states/outcomes in
//! vike-app, which CI only compile-checks) and the opt-in venue-native (resting maker TP / native
//! stop) exits — this stage keeps exits emulated/portable (a plain opposite-side market on the
//! barrier hit, reusing `order_fill_price`), the parity-clean v1.

use vike_model::{
    Bar, Broker, ControllerParams, Fill, OrderLifecycle, QuoteTick, Strategy, StrategyParams,
    TradeTick, TripleBarrier,
};

use crate::position_executor::{ExecutorOutcome, PositionExecutor, PositionIntent};
use toml::Value;

/// Read a TOML value as `f64`, accepting a TOML float OR integer (`qty = 1` == `qty = 1.0`) — the
/// lenient numeric reader convention shared with the backtest registry + `grid_dca`. Feeds both
/// controllers' `from_params`.
pub(crate) fn as_f64(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

/// Read a [`TripleBarrier`] from a harness/registry params table — every leg OPTIONAL (absent ⇒
/// un-armed): `tp`/`sl`/`trailing` are absolute price offsets from the entry fill and `time_limit_ms`
/// the max holding duration (see [`TripleBarrier::new`]). Shared by [`MomentumController::from_params`]
/// and [`crate::funding_carry::FundingCarryController::from_params`].
pub(crate) fn barriers_from_params(params: &Value) -> TripleBarrier {
    TripleBarrier::new(
        params.get("tp").and_then(as_f64),
        params.get("sl").and_then(as_f64),
        params.get("time_limit_ms").and_then(Value::as_integer),
        params.get("trailing").and_then(as_f64),
    )
}

/// The signal → intent brain (the WHAT). A pure decision function the [`ControllerHarness`] calls to
/// decide whether to OPEN a position for one `(venue, symbol)`; `None` = do nothing.
///
/// It reads the SAME market/account view a [`Strategy`] gets — but through a SHARED `&B`, so it can
/// call [`Broker::price`] / [`Broker::bars`] / [`Broker::position`] / [`Broker::now`] and yet
/// **structurally cannot submit orders** (the `submit_*` verbs need `&mut B`): a controller decides,
/// the executor executes (spec §3.1). It reasons in POSITIONS (emits a [`PositionIntent`] — side,
/// qty, entry, triple barrier, refresh/retry), never in raw orders, and never sees fills.
///
/// The harness invokes `evaluate` ONLY when it is free to open — no active executor for this
/// `(venue, symbol)` AND the post-exit cooldown has elapsed — so an implementation need not re-check
/// those; it only decides direction/size/barriers. Generic over `B` (monomorphized per engine, like
/// [`Strategy`] itself); mutable `&mut self` so a controller may carry its own state.
pub trait Controller {
    /// Decide whether to open a position for `(venue, symbol)` now. `Some(intent)` opens it; `None`
    /// declines. `venue`/`symbol` name the market the harness is asking about (the controller echoes
    /// them into the [`PositionIntent`] and may key `broker` reads by `symbol`).
    fn evaluate<B: Broker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionIntent>;

    /// Absorb a LIVE-PARAMS re-tune (position-executor stage 6). The [`ControllerHarness`] calls this
    /// from its [`Strategy::on_params_updated`] AFTER applying the harness-level cooldown, handing the
    /// FULL [`ControllerParams`] bag; a controller picks out the knobs it owns (its intent template +
    /// decision knobs) and hot-swaps them in place — the change takes effect on the controller's NEXT
    /// [`evaluate`](Controller::evaluate) (i.e. only FUTURE opens; an in-flight executor is untouched).
    ///
    /// DEFAULT NO-OP so the trait gains this method additively — every existing [`Controller`] keeps
    /// compiling and behaving identically until it overrides this. Not generic over `B` (a re-tune
    /// touches no broker): a plain `&mut self` field swap, atomic on the single core thread.
    fn apply_params(&mut self, params: &ControllerParams) {
        let _ = params;
    }
}

/// The live tunables of a [`ControllerHarness`] mount — the payload of a
/// A minimal REFERENCE [`Controller`] (an example + the harness's test exemplar, NOT a trading
/// recommendation): a price-momentum threshold. Each time it is invited (recall: only while the pair
/// is flat AND past cooldown) it compares the current mark ([`Broker::price`]) to the mark at its
/// PREVIOUS invitation; if the move is `>= threshold` it opens LONG, `<= -threshold` SHORT, otherwise
/// it declines (`None`). The first-ever invitation has no reference, so it only records the mark and
/// declines. Deterministic and read-only — it works identically on the bar path (`price` = bar close)
/// and the tick path (`price` = last tick mark), and its internal reference evolves identically live
/// and in backtest (the invitation points and marks are bit-identical), so a harness mounting it is
/// parity-clean.
#[derive(Debug, Clone)]
pub struct MomentumController {
    /// Position size (units) each opened intent carries.
    pub qty: f64,
    /// Absolute price move (since the last invitation) that arms a long (`>=`) / short (`<= -`).
    /// `0.0` ⇒ always open from the second invitation on (up/flat → long, down → short).
    pub threshold: f64,
    /// The triple barrier every opened position is guarded by (market entry, emulated exits).
    pub barriers: TripleBarrier,
    /// The mark observed at the previous invitation (`None` before the first) — the momentum origin.
    last_ref: Option<f64>,
}

impl MomentumController {
    /// A momentum controller sizing every entry at `qty`, arming on a `threshold` move, guarding each
    /// position with `barriers`. Starts with no reference (declines its first invitation).
    pub fn new(qty: f64, threshold: f64, barriers: TripleBarrier) -> Self {
        MomentumController { qty, threshold, barriers, last_ref: None }
    }

    /// Read a harness/registry TOML params table into a controller (the `BuyHold::from_params` reader
    /// convention — unknown keys ignored, missing keys default): `qty` (default `1.0`), `threshold`
    /// (default `0.0`), and the triple-barrier legs `tp`/`sl`/`time_limit_ms`/`trailing` (all optional,
    /// absent ⇒ un-armed — see [`barriers_from_params`]). The backtest registry wraps the result in a
    /// [`ControllerHarness`] to make it a `Strategy` (the harness-level `venue`/`cooldown_ms` knobs are
    /// read there, not here).
    pub fn from_params(params: &Value) -> Self {
        let qty = params.get("qty").and_then(as_f64).unwrap_or(1.0);
        let threshold = params.get("threshold").and_then(as_f64).unwrap_or(0.0);
        MomentumController::new(qty, threshold, barriers_from_params(params))
    }
}

impl Controller for MomentumController {
    fn evaluate<B: Broker>(
        &mut self,
        broker: &B,
        venue: &str,
        symbol: &str,
    ) -> Option<PositionIntent> {
        let px = broker.price(symbol);
        let side = match self.last_ref {
            Some(prev) if px - prev >= self.threshold => 1,
            Some(prev) if px - prev <= -self.threshold => -1,
            _ => 0, // no reference yet, or the move is inside the threshold band
        };
        self.last_ref = Some(px);
        if side == 0 {
            return None;
        }
        Some(PositionIntent::market(venue, symbol, side, self.qty, self.barriers))
    }

    /// Hot-swap this controller's tunables from a live-params re-tune (stage 6): its intent-template
    /// size + triple barrier and its momentum `threshold`. The `cooldown_ms` field is the HARNESS's,
    /// applied by the harness itself, so it is ignored here. The momentum origin `last_ref` is
    /// DELIBERATELY preserved — it is evolving STATE (the price at the previous invitation), not a
    /// tunable, so a re-tune re-uses the current reference and the new `threshold` takes effect on the
    /// very next [`evaluate`](Controller::evaluate) without a spurious first-invitation reset.
    fn apply_params(&mut self, params: &ControllerParams) {
        self.qty = params.qty;
        self.threshold = params.threshold;
        self.barriers = params.barriers;
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

/// The controller → executor GLUE, and itself a [`Strategy`] (spec §3.1/§3.4). It owns the
/// [`Controller`], the active [`PositionExecutor`]s (one per `(venue, symbol)`), and the per-pair
/// post-exit cooldown — see the module docs for the per-callback flow, the parity argument, and what
/// is deferred to stage 6.
///
/// Mount it like any strategy: `Box::new(ControllerHarness::new(controller, venue, cooldown_ms))` as a
/// live `StrategyMount`, or `StrategyEngine::new(bars, ControllerHarness::new(..), params)` in
/// backtest. `C: Send` (the reference [`MomentumController`] is) makes it a valid
/// `Box<dyn Strategy<LiveBroker> + Send>`.
#[derive(Debug)]
pub struct ControllerHarness<C: Controller> {
    controller: C,
    /// The DEFAULT venue this harness operates under — the `venue` half of an executor key for a
    /// symbol NOT in [`venue_map`](Self::venue_map), and the value echoed into a produced intent.
    venue: String,
    /// Optional per-symbol venue routing (`symbol -> venue`): when a bar/tick/fill for `symbol`
    /// arrives, the harness keys its executor + asks [`Controller::evaluate`] under
    /// `venue_map[symbol]` instead of the default `venue`. This is what makes a CROSS-VENUE controller
    /// (e.g. `FundingCarryController`, whose carry needs ≥2 venues' funding in its book) tradeable
    /// from ONE mount: each venue's series routes to its own venue, so the controller observes them
    /// all and can open the carry. EMPTY (the default) ⇒ every symbol falls back to `venue` and the
    /// harness is byte-identical to the single-venue mount. Insertion order (few entries → linear
    /// scan, no map dep, deterministic replay — same convention as `executors`/`cooldowns`).
    venue_map: Vec<(String, String)>,
    /// Post-exit cooldown (ms): the controller may not re-open a `(venue, symbol)` until
    /// `last_close_ts + cooldown_ms`. `0` ⇒ eligible to re-open on the very next observation.
    cooldown_ms: i64,
    /// Active executors, one per `(venue, symbol)`, in insertion order (few → linear scan, no map dep,
    /// deterministic replay).
    executors: Vec<((String, String), PositionExecutor)>,
    /// `last_close_ts` per `(venue, symbol)` — the cooldown origin, stamped when an executor is reaped.
    cooldowns: Vec<((String, String), i64)>,
    /// Terminal [`ExecutorOutcome`]s in completion order (a closed round-trip records one; a
    /// `Failed`/`Canceled` executor records none). The harness's inspectable result trail.
    outcomes: Vec<ExecutorOutcome>,
    /// Live-params generation counter (stage 6): starts at `0`, bumped once per APPLIED
    /// [`StrategyParams::PositionController`] update via [`on_params_updated`](Self::on_params_updated).
    /// Observable ([`params_epoch`](Self::params_epoch)) so a caller/test can confirm a re-tune landed;
    /// a foreign variant does NOT bump it. Mirrors `SpreadMaker::params_epoch`.
    params_epoch: u64,
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
        }
    }

    /// Set the per-symbol venue routing (`symbol -> venue`) — see [`venue_map`](Self::venue_map).
    /// A symbol NOT present routes to the default `venue`. Overwrites any prior map. Empty ⇒
    /// single-venue behavior (byte-identical). Builder form so a mount reads it once at construction.
    pub fn with_venue_map(mut self, venue_map: Vec<(String, String)>) -> Self {
        self.venue_map = venue_map;
        self
    }

    /// Resolve the venue this `symbol` trades under: its [`venue_map`](Self::venue_map) entry, or the
    /// default `venue` when unmapped (the single-venue case). The `(venue, symbol)` this returns is
    /// the executor key AND the venue passed to [`Controller::evaluate`] — so a cross-venue controller
    /// observes each series under its OWN venue.
    fn venue_for(&self, symbol: &str) -> &str {
        self.venue_map
            .iter()
            .find(|(s, _)| s == symbol)
            .map(|(_, v)| v.as_str())
            .unwrap_or(&self.venue)
    }

    /// The terminal outcomes recorded so far (one per CLOSED round-trip, completion order).
    pub fn outcomes(&self) -> &[ExecutorOutcome] {
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

    /// The live-params generation (stage 6): `0` before any update, bumped once per APPLIED
    /// [`StrategyParams::PositionController`] re-tune. A GUI/test reads this to confirm a re-tune
    /// landed; a foreign-variant update leaves it unchanged.
    pub fn params_epoch(&self) -> u64 {
        self.params_epoch
    }

    // ---- internals ----

    /// Step 1 — ask the controller to open a position for `symbol`, iff there is no active executor
    /// for it and its cooldown has elapsed. Spawns + starts the executor (which submits the entry).
    fn maybe_open<B: Broker>(&mut self, broker: &mut B, symbol: &str) {
        if symbol.is_empty() {
            return; // an un-tagged event can't be keyed/routed — decline (fail-safe, no open)
        }
        let key = (self.venue_for(symbol).to_string(), symbol.to_string());
        if self.executors.iter().any(|(k, _)| *k == key) {
            return; // one executor per (venue, symbol) — already live
        }
        let now = broker.now();
        if !self.cooldown_ok(&key, now) {
            return; // still cooling down since the last close
        }
        if let Some(intent) = self.controller.evaluate(&*broker, &key.0, &key.1) {
            let mut executor = PositionExecutor::new(intent);
            executor.start(broker); // submits the entry through the SAME broker → drain → gate
            self.executors.push((key, executor));
        }
    }

    /// Steps 2+3 — advance the executor for `symbol` (refresh/retry timer, then the barrier check for
    /// this observation), then reap it if it went terminal.
    fn drive<B: Broker>(&mut self, broker: &mut B, symbol: &str, event: StepEvent) {
        let key = (self.venue_for(symbol).to_string(), symbol.to_string());
        let Some(idx) = self.executors.iter().position(|(k, _)| *k == key) else {
            return;
        };
        {
            let executor = &mut self.executors[idx].1;
            executor.on_refresh(broker); // retry a backed-off entry / reprice a resting limit
            match event {
                StepEvent::Bar(bar) => executor.on_bar(broker, bar),
                StepEvent::Tick(px) => executor.on_tick(broker, px),
            }
        }
        self.reap_terminals(broker.now());
    }

    /// Step 3 — drop every executor that has reached a terminal state, recording its outcome and
    /// stamping the pair's cooldown. The cooldown origin is the executor's `exit_ts` for a closed
    /// round-trip (bit-identical across runtimes) and `now` for a no-position terminal
    /// (`Failed`/`Canceled`).
    fn reap_terminals(&mut self, now: i64) {
        let mut i = 0;
        while i < self.executors.len() {
            if self.executors[i].1.is_terminal() {
                let (key, executor) = self.executors.remove(i);
                let outcome = executor.outcome();
                let close_ts = outcome.map(|o| o.exit_ts).unwrap_or(now);
                if let Some(o) = outcome {
                    self.outcomes.push(o);
                }
                self.set_cooldown(key, close_ts);
            } else {
                i += 1;
            }
        }
    }

    /// `true` if `key` may open now: no cooldown recorded, or `now` has reached `last_close + cooldown`.
    fn cooldown_ok(&self, key: &(String, String), now: i64) -> bool {
        match self.cooldowns.iter().find(|(k, _)| k == key) {
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

#[allow(unused_variables)]
impl<B: Broker, C: Controller> Strategy<B> for ControllerHarness<C> {
    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let symbol = bar.symbol.clone().unwrap_or_default();
        self.maybe_open(broker, &symbol);
        self.drive(broker, &symbol, StepEvent::Bar(bar));
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let symbol = q.symbol.clone();
        self.maybe_open(broker, &symbol);
        self.drive(broker, &symbol, StepEvent::Tick(q.mid()));
    }

    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {
        let symbol = t.symbol.clone();
        self.maybe_open(broker, &symbol);
        self.drive(broker, &symbol, StepEvent::Tick(t.price));
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        let key = (self.venue_for(&fill.symbol).to_string(), fill.symbol.clone());
        if let Some(idx) = self.executors.iter().position(|(k, _)| *k == key) {
            self.executors[idx].1.on_fill(fill);
        }
        self.reap_terminals(broker.now());
    }

    fn on_order_event(&mut self, broker: &mut B, event: &OrderLifecycle) {
        // The portable Broker surfaces no client-order-id, so a single order can't be routed here;
        // broadcast to the active executors (one per pair). A non-reject transition is a no-op inside
        // the executor, so this is harmless; a reject drives the owning executor's retry/refresh.
        for (_key, executor) in self.executors.iter_mut() {
            executor.on_order_event(broker, event);
        }
        self.reap_terminals(broker.now());
    }

    /// LIVE-PARAMS re-tune (stage 6): hot-swap the harness cooldown + the controller's tunables
    /// ATOMICALLY and bump [`params_epoch`](Self::params_epoch) — WITHOUT unmounting (the runtime
    /// routes this off `Command::UpdateParams` → `drive_strategy_params`; see the module docs). The
    /// broker is intentionally UNTOUCHED: no order is placed/canceled here, and — critically — the
    /// active [`PositionExecutor`]s are NOT disturbed. A re-tune affects only FUTURE opens: the next
    /// [`Controller::evaluate`] mints intents at the new size/barriers, while every IN-FLIGHT executor
    /// keeps the intent/barriers it was armed with (never torn down, never retroactively resized) —
    /// the "re-tune doesn't disturb resting work" rule the maker's live-params also follow.
    ///
    /// A foreign [`StrategyParams`] variant (e.g. [`StrategyParams::SpreadMaker`]) is a NO-OP: it is
    /// not ours to consume, so nothing swaps and the epoch does not move. The refutable `let ... else`
    /// (vs the maker's single-variant irrefutable `let`) is what makes another family's update a clean
    /// no-op here rather than a misconsumed swap. Runs on the single core thread between ticks, so the
    /// whole swap is atomic w.r.t. dispatch.
    fn on_params_updated(&mut self, _broker: &mut B, params: &StrategyParams) {
        let StrategyParams::PositionController(p) = params else {
            return; // not our variant — ignore (additive; SpreadMaker etc. are untouched)
        };
        self.cooldown_ms = p.cooldown_ms; // harness-level knob
        self.controller.apply_params(p); // controller-level knobs (size / barriers / threshold)
        self.params_epoch += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::position_executor::{BarrierKind, ExecutorState};
    use vike_model::{QuoteStyle, SpreadMakerParams};
    // The recording `Broker` test double is shared from vike-model (behind its `test-support`
    // feature) — the same mock `position_executor`'s machine tests drive. This harness is proven
    // generic over the `Broker` TRAIT, never wired to a concrete engine broker. `position()` reads
    // the mock's `pos` (0 by default here, matching the prior local mock's hardcoded flat read).
    use vike_model::strategy::MockBroker;

    fn bar(ts: i64, symbol: &str, open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            ts,
            open,
            high,
            low,
            close,
            volume: 0.0,
            funding: None,
            bid: None,
            ask: None,
            symbol: Some(symbol.to_string()),
        }
    }

    fn fill(symbol: &str, side: i32, size: f64, price: f64, ts: i64) -> Fill {
        Fill { side, size, price, fee: 0.0, ts, is_maker: false, symbol: symbol.to_string() }
    }

    /// qty 1, threshold 0 (always opens from the 2nd invitation), TP +10.
    fn harness() -> ControllerHarness<MomentumController> {
        let ctl =
            MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(10.0), None, None, None));
        ControllerHarness::new(ctl, "v", 0)
    }

    #[test]
    fn first_invitation_declines_no_reference() {
        let mut h = harness();
        let mut b = MockBroker { now: 1, px: 100.0, ..Default::default() };
        // the momentum controller has no reference yet → declines; no executor, no order
        Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0));
        assert_eq!(h.active_count(), 0);
        assert!(b.markets.is_empty());
    }

    #[test]
    fn opens_one_executor_on_signal_and_not_twice() {
        let mut h = harness();
        let mut b = MockBroker { now: 1, px: 100.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // sets ref 100
        b.now = 2;
        b.px = 105.0; // +5 move ≥ threshold 0 → open long
        Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 105.0, 105.0, 105.0, 105.0));
        assert_eq!(h.active_count(), 1, "one executor opened");
        assert_eq!(b.markets, vec![("S".to_string(), 1, 1.0)], "a single long market entry");
        // a further bar while the executor is live must NOT open a second (one per (venue, symbol))
        b.now = 3;
        b.px = 110.0;
        Strategy::on_bar(&mut h, &mut b, &bar(3, "S", 110.0, 110.0, 110.0, 110.0));
        assert_eq!(h.active_count(), 1, "still exactly one executor");
        assert_eq!(b.markets.len(), 1, "no second entry while one is live");
    }

    #[test]
    fn full_lifecycle_through_the_harness_records_outcome() {
        let mut h = harness();
        let mut b = MockBroker { now: 1, px: 100.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // ref
        b.now = 2;
        b.px = 101.0;
        Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 101.0, 101.0, 101.0, 101.0)); // open long
        assert_eq!(h.active_count(), 1);
        // entry fills @100 → Open (routed by fill.symbol)
        Strategy::on_fill(&mut h, &mut b, &fill("S", 1, 1.0, 100.0, 3));
        // a bar rallying through the TP target (100+10=110) fires TP → close submitted
        b.now = 4;
        Strategy::on_bar(&mut h, &mut b, &bar(4, "S", 105.0, 112.0, 104.0, 111.0));
        assert_eq!(b.markets, vec![("S".to_string(), 1, 1.0), ("S".to_string(), -1, 1.0)]);
        // the close fills @110 → Closed → reaped: outcome recorded, executor dropped, cooldown set
        Strategy::on_fill(&mut h, &mut b, &fill("S", -1, 1.0, 110.0, 5));
        assert_eq!(h.active_count(), 0, "closed executor reaped");
        assert_eq!(h.outcomes().len(), 1);
        let o = h.outcomes()[0];
        assert_eq!(o.barrier_hit, Some(BarrierKind::TakeProfit));
        assert_eq!(o.entry_px, 100.0);
        assert_eq!(o.exit_px, 110.0);
        assert_eq!(o.entry_ts, 3);
        assert_eq!(o.exit_ts, 5);
        assert_eq!(o.realized_pnl, 10.0); // (110-100)*(+1)*1
    }

    #[test]
    fn cooldown_gates_the_next_open() {
        // cooldown 1000ms: after a close at ts=5 the pair may not re-open until ts>=1005
        let ctl =
            MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(10.0), None, None, None));
        let mut h = ControllerHarness::new(ctl, "v", 1000);
        let mut b = MockBroker { now: 1, px: 100.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0));
        b.now = 2;
        b.px = 101.0;
        Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 101.0, 101.0, 101.0, 101.0));
        Strategy::on_fill(&mut h, &mut b, &fill("S", 1, 1.0, 100.0, 3));
        b.now = 4;
        Strategy::on_bar(&mut h, &mut b, &bar(4, "S", 105.0, 112.0, 104.0, 111.0)); // TP
        Strategy::on_fill(&mut h, &mut b, &fill("S", -1, 1.0, 110.0, 5)); // close @ts5
        assert_eq!(h.active_count(), 0);
        let entries_after_close = b.markets.len();
        // within the cooldown window: no re-open even on a fresh signal
        b.now = 500;
        b.px = 130.0;
        Strategy::on_bar(&mut h, &mut b, &bar(6, "S", 130.0, 130.0, 130.0, 130.0));
        assert_eq!(h.active_count(), 0, "cooldown blocks re-open");
        assert_eq!(b.markets.len(), entries_after_close, "no new entry during cooldown");
        // past the cooldown (5 + 1000 = 1005): a signal re-opens
        b.now = 1005;
        b.px = 140.0;
        Strategy::on_bar(&mut h, &mut b, &bar(7, "S", 140.0, 140.0, 140.0, 140.0));
        assert_eq!(h.active_count(), 1, "re-opens once the cooldown elapsed");
        assert_eq!(b.markets.len(), entries_after_close + 1);
    }

    #[test]
    fn fills_route_by_symbol_across_two_pairs() {
        // two symbols → two independent executors; a fill only advances its own.
        let ctl =
            MomentumController::new(1.0, 0.0, TripleBarrier::new(Some(10.0), None, None, None));
        let mut h = ControllerHarness::new(ctl, "v", 0);
        let mut b = MockBroker { now: 1, px: 100.0, ..Default::default() };
        // prime + open A
        Strategy::on_bar(&mut h, &mut b, &bar(1, "A", 100.0, 100.0, 100.0, 100.0));
        b.now = 2;
        b.px = 101.0;
        Strategy::on_bar(&mut h, &mut b, &bar(2, "A", 101.0, 101.0, 101.0, 101.0));
        // prime + open B (shared controller reference is per-invocation; both open long on the rise)
        Strategy::on_bar(&mut h, &mut b, &bar(2, "B", 100.0, 100.0, 100.0, 100.0));
        b.px = 102.0;
        Strategy::on_bar(&mut h, &mut b, &bar(3, "B", 102.0, 102.0, 102.0, 102.0));
        assert_eq!(h.active_count(), 2, "one executor per symbol");
        // a fill for A only opens A; B stays EntryWorking
        Strategy::on_fill(&mut h, &mut b, &fill("A", 1, 1.0, 100.0, 4));
        let a = h.executors.iter().find(|(k, _)| k.1 == "A").unwrap();
        let b_ex = h.executors.iter().find(|(k, _)| k.1 == "B").unwrap();
        assert_eq!(a.1.state(), ExecutorState::Open, "A opened on its fill");
        assert_eq!(b_ex.1.state(), ExecutorState::EntryWorking, "B unaffected by A's fill");
    }

    // ====================================================================================
    // STAGE 6 — live-params (StrategyParams::PositionController + on_params_updated hot-swap)
    // ====================================================================================

    /// A full [`SpreadMakerParams`] — the FOREIGN variant used to prove the harness ignores it.
    fn a_spreadmaker_params() -> SpreadMakerParams {
        SpreadMakerParams {
            qty: 9.0,
            half_spread: 9.0,
            target_inventory: 0.0,
            max_inventory: 1.0,
            skew: 0.0,
            fill_window_ms: 0,
            net_fill_threshold: 0.0,
            suppress_cooldown_ms: 0,
            style: QuoteStyle::Mid,
            depth_levels: 1,
            tick_size: 0.0,
            filter_own: false,
            avellaneda_stoikov: None,
            refresh_tolerance: None,
            ladder: None,
            reward: None,
            toxicity: None,
        }
    }

    #[test]
    fn controller_params_round_trips_serde() {
        let p = ControllerParams::new(
            1_500,
            2.5,
            TripleBarrier::new(Some(10.0), Some(4.0), Some(60_000), Some(2.0)),
            3.5,
        );
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(p, serde_json::from_str::<ControllerParams>(&json).unwrap());
        // and wrapped in the journaled StrategyParams union (the shape Command::UpdateParams carries)
        let sp = StrategyParams::PositionController(p);
        let j2 = serde_json::to_string(&sp).unwrap();
        assert_eq!(sp, serde_json::from_str::<StrategyParams>(&j2).unwrap());
    }

    #[test]
    fn on_params_updated_hot_swaps_controller_and_bumps_epoch() {
        let mut h = harness(); // qty 1, threshold 0, cooldown 0, TP+10
        assert_eq!(h.params_epoch(), 0, "starts at epoch 0");
        assert_eq!(h.controller().qty, 1.0);
        assert_eq!(h.controller().threshold, 0.0);

        // re-tune: qty 1→3, threshold 0→5, cooldown 0→2000, TP 10→25
        let mut b = MockBroker::default();
        let p = ControllerParams::new(
            2_000,
            3.0,
            TripleBarrier::new(Some(25.0), None, None, None),
            5.0,
        );
        Strategy::on_params_updated(&mut h, &mut b, &StrategyParams::PositionController(p));

        assert_eq!(h.params_epoch(), 1, "epoch bumped once by the applied re-tune");
        assert_eq!(h.controller().qty, 3.0, "controller size hot-swapped");
        assert_eq!(h.controller().threshold, 5.0, "controller threshold hot-swapped");
        assert_eq!(h.controller().barriers.take_profit, Some(25.0), "barriers hot-swapped");
        assert_eq!(h.cooldown_ms, 2_000, "harness cooldown hot-swapped");

        // the swap takes effect on the NEXT evaluate: with the new threshold 5, a +4 move no longer
        // opens, but a +6 move does — and the entry then uses the new size 3.
        let mut b2 = MockBroker { now: 1, px: 100.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b2, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // ref 100
        b2.now = 2;
        b2.px = 104.0; // +4 < new threshold 5 → decline
        Strategy::on_bar(&mut h, &mut b2, &bar(2, "S", 104.0, 104.0, 104.0, 104.0));
        assert_eq!(h.active_count(), 0, "the new threshold gates the open (swap took effect)");
        b2.now = 3;
        b2.px = 110.0; // +6 ≥ new threshold 5 → open long at the new size
        Strategy::on_bar(&mut h, &mut b2, &bar(3, "S", 110.0, 110.0, 110.0, 110.0));
        assert_eq!(h.active_count(), 1);
        assert_eq!(b2.markets, vec![("S".to_string(), 1, 3.0)], "entry uses the hot-swapped size");
    }

    #[test]
    fn on_params_updated_is_a_noop_for_a_foreign_variant() {
        let mut h = harness(); // qty 1, threshold 0, cooldown 0
        let mut b = MockBroker::default();
        // a SpreadMaker update is NOT ours: nothing swaps, the epoch does not move.
        Strategy::on_params_updated(
            &mut h,
            &mut b,
            &StrategyParams::SpreadMaker(a_spreadmaker_params()),
        );
        assert_eq!(h.params_epoch(), 0, "a foreign variant does not bump the epoch");
        assert_eq!(h.controller().qty, 1.0, "controller size untouched by a foreign variant");
        assert_eq!(h.controller().threshold, 0.0, "controller threshold untouched");
        assert_eq!(h.cooldown_ms, 0, "harness cooldown untouched");
    }

    #[test]
    fn retune_does_not_tear_down_an_in_flight_executor() {
        let mut h = harness(); // qty 1, threshold 0, cooldown 0, TP+10
        let mut b = MockBroker { now: 1, px: 100.0, ..Default::default() };
        Strategy::on_bar(&mut h, &mut b, &bar(1, "S", 100.0, 100.0, 100.0, 100.0)); // ref
        b.now = 2;
        b.px = 101.0;
        Strategy::on_bar(&mut h, &mut b, &bar(2, "S", 101.0, 101.0, 101.0, 101.0)); // open long, qty 1
        assert_eq!(h.active_count(), 1);
        assert_eq!(b.markets, vec![("S".to_string(), 1, 1.0)], "entry at the ORIGINAL size 1");

        // re-tune mid-flight to a very different size/barrier while the entry is still working.
        let p =
            ControllerParams::new(0, 9.0, TripleBarrier::new(Some(50.0), None, None, None), 0.0);
        Strategy::on_params_updated(&mut h, &mut b, &StrategyParams::PositionController(p));

        // the in-flight executor SURVIVES and KEEPS its original intent — a re-tune only affects
        // FUTURE opens; it is never torn down or retroactively resized.
        assert_eq!(h.active_count(), 1, "in-flight executor not torn down by the re-tune");
        let (_, ex) = h.executors.iter().find(|(k, _)| k.1 == "S").unwrap();
        assert_eq!(ex.state(), ExecutorState::EntryWorking, "still working its entry");
        assert_eq!(ex.intent().qty, 1.0, "in-flight keeps its ORIGINAL size (not resized to 9)");
        assert_eq!(
            ex.intent().barriers.take_profit,
            Some(10.0),
            "in-flight keeps its ORIGINAL barriers (not re-armed to +50)"
        );
        assert_eq!(h.params_epoch(), 1, "the re-tune still landed (epoch bumped)");
        assert_eq!(b.markets.len(), 1, "no extra order minted by the re-tune (broker untouched)");
    }
}
