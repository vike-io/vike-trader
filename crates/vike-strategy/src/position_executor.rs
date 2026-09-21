//! position-executor STAGE 1 — the position-lifecycle TYPES + the triple-barrier MATH.
//!
//! Design: `docs/superpowers/specs/2026-07-11-position-executor.md` (PR1). A Hummingbot-style
//! *controller → PositionExecutor* split, re-expressed on vike's `Strategy`/`Broker` seam. This
//! module is the bottom, PURE half: the data types a controller emits ([`PositionIntent`],
//! [`TripleBarrier`], [`EntryKind`], [`RefreshPolicy`], [`RetryPolicy`]), the executor's state
//! labels + terminal record ([`ExecutorState`], [`ExecutorOutcome`]), and the pure barrier-check
//! free functions ([`evaluate_barriers`], [`time_barrier_hit`], …). RUST-NATIVE — no Python twin
//! (like [`vike_model::BracketSpec`]); it lives in `vike-model` (not `vike-core`) precisely because the
//! executor must run bit-identically on BOTH engines, and `vike-model` is the only crate the
//! backtest and live stacks share downward (spec §3.3).
//!
//! STAGE 2 ADDED the [`PositionExecutor`] state machine below (the `start`/`on_bar`/`on_tick`/
//! `on_fill` transition logic that drives ONE position `Pending → EntryWorking → Open → Closing →
//! Closed`, plus the `Failed`/`Canceled` terminals — driven over a portable `&mut B: Broker`).
//! STAGE 3 added the `Strategy::on_order_event` hook + `vike-core` coid→mount routing (in
//! `strategy.rs` and the runtime) that delivers the venue reject/deny the executor reacts to.
//! STAGE 4 (this change) makes the executor ACT on the carried policies: the [`RefreshPolicy`] timer
//! ([`PositionExecutor::on_refresh`] reprices a still-working LIMIT entry every `every_ms`), the
//! [`RetryPolicy`] budget + backoff ([`PositionExecutor::on_order_rejected`] resubmits a rejected
//! entry up to `max_attempts`, spaced by `backoff_ms`, terminalizing `Failed` only once exhausted),
//! and the sticky close-reject RESUBMIT (a rejected CLOSE re-submits and stays `Closing`). STILL
//! DEFERRED: the `Controller` trait + `ControllerHarness` (the `Strategy` glue that drives these step
//! methods, stage 5), the post-exit COOLDOWN (a CONTROLLER-level "next-open-not-before" gate that
//! sequences successive executors — NOT this single-position executor's concern, stage 5), and any
//! backtest/live wiring + parity gate (stage 6). The free-standing types above stay DATA only.
//!
//! ## Barrier conventions (the wire shape, frozen here — spec Q6)
//! - **take-profit / stop-loss / trailing** are ABSOLUTE PRICE OFFSETS (positive distance in price
//!   units) from the ENTRY FILL price. The signed barrier *price* is derived at check time from the
//!   entry price + the position side ([`take_profit_price`] / [`stop_loss_price`]). A basis-point
//!   convenience constructor ([`TripleBarrier::from_bps`]) converts bps → offset against a reference
//!   price for callers that prefer relative sizing.
//! - **time_limit_ms** is a max HOLDING duration: the position is force-exited once
//!   `now >= entry_fill_ts + time_limit_ms` (the genuinely-new third barrier — nothing else in the
//!   codebase closes a *position* on elapsed time). Time is an INJECTED `now_ms` (`broker.now()`),
//!   never a wall clock, so the predicate is deterministic and reproduces in backtest (spec Q5).
//!
//! ## Emulated, portable exits (spec §3.2, Q1)
//! Every barrier is detected over the common [`vike_model::order_fill_price`] law — the same gap-open
//! adverse-fill / trailing-ratchet oracle the `ConditionalBook` emulator and the backtest engines
//! use — so a barrier hit behaves bit-identically live and in backtest, and the reported `exit_px`
//! is the oracle's trigger fill (a plain market-close reuses the SAME law later). Nothing here bakes
//! in a venue-native resting exit; that richer path is a later opt-in.
//!
//! ## Realized PnL law (dedup task 7)
//! [`ExecutorOutcome::realized_pnl`] is folded through the canonical [`vike_model::TradeFold`] —
//! the ONE cost-basis primitive `Account`/`SimBroker`/the vector engine/`recon` share — rather than
//! a hand-rolled VWAP-diff, so it inherits the contract multiplier ([`PositionExecutor::with_multiplier`])
//! and the GROSS convention (fees reported separately via [`ExecutorOutcome::fees`] /
//! [`ExecutorOutcome::net`]) for free. See the `fold`/`multiplier` field docs on [`PositionExecutor`].

use serde::{Deserialize, Serialize};

use vike_model::{
    Bar, Broker, Fill, OrderEventKind, OrderKind, OrderLifecycle, TradeFold, TripleBarrier,
    WorkingOrder, one_price_bar, order_fill_price,
};

/// How the executor gets INTO a position.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// Cross the spread now — a plain market entry.
    #[default]
    Market,
    /// Rest a limit at `price` and wait for the fill (subject to [`RefreshPolicy`], later stage).
    Limit { price: f64 },
}

/// How an unfilled LIMIT entry is refreshed while it rests (DATA only — the timer/behaviour is a
/// later stage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefreshPolicy {
    /// Re-evaluate the resting entry every this-many ms (against `broker.now()`).
    pub every_ms: i64,
    /// What the refresh does when it fires.
    pub mode: RefreshMode,
}

/// The action a [`RefreshPolicy`] takes on an unfilled entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshMode {
    /// Re-price the resting entry IN PLACE (preserves venue queue priority where the venue allows).
    #[default]
    Reprice,
    /// Cancel the resting entry and submit a fresh one.
    CancelReplace,
}

impl RefreshPolicy {
    pub fn new(every_ms: i64, mode: RefreshMode) -> Self {
        RefreshPolicy { every_ms, mode }
    }
}

/// How a REJECTED / DENIED entry submission is retried (DATA only — the counter/backoff is a later
/// stage).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetryPolicy {
    /// Maximum TOTAL entry submissions (including the first). `1` = submit once, never retry.
    pub max_attempts: u32,
    /// Delay (ms) between successive attempts.
    pub backoff_ms: i64,
}

impl Default for RetryPolicy {
    /// Single attempt, no backoff — the "don't retry" default.
    fn default() -> Self {
        RetryPolicy { max_attempts: 1, backoff_ms: 0 }
    }
}

impl RetryPolicy {
    pub fn new(max_attempts: u32, backoff_ms: i64) -> Self {
        RetryPolicy { max_attempts, backoff_ms }
    }

    /// A single submit with no retry (same as [`Default`]).
    pub fn none() -> Self {
        RetryPolicy::default()
    }
}

/// A controller's *position intent* — "open a `qty` `side` position in `(venue, symbol)`, enter via
/// `entry`, protect it with `barriers`, refresh/retry per these policies" (spec §3.1). The natural
/// payload of a future live-params variant. Pure serde; `#[serde(default)]` on the policy fields
/// keeps a minimal `{venue,symbol,side,qty}` JSON valid (additive).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PositionIntent {
    pub venue: String,
    /// canonical symbol (resolver maps to the venue symbol at the edge)
    pub symbol: String,
    /// position direction: +1 long / -1 short
    pub side: i32,
    /// position size in units
    pub qty: f64,
    /// how to get IN
    #[serde(default)]
    pub entry: EntryKind,
    /// how to get OUT (the triple barrier)
    #[serde(default)]
    pub barriers: TripleBarrier,
    /// re-price/cancel an unfilled limit entry (`None` = leave it resting)
    #[serde(default)]
    pub refresh: Option<RefreshPolicy>,
    /// re-submit a rejected/denied entry
    #[serde(default)]
    pub retry: RetryPolicy,
}

impl PositionIntent {
    /// A market-entry intent with barriers and default (no-retry) policies.
    pub fn market(
        venue: impl Into<String>,
        symbol: impl Into<String>,
        side: i32,
        qty: f64,
        barriers: TripleBarrier,
    ) -> Self {
        PositionIntent {
            venue: venue.into(),
            symbol: symbol.into(),
            side,
            qty,
            entry: EntryKind::Market,
            barriers,
            refresh: None,
            retry: RetryPolicy::default(),
        }
    }
}

/// The `PositionExecutor`'s lifecycle STATE label (spec §3.1). DATA only here — the transitions (the
/// state machine) are a later stage. `NOT_STARTED → RUNNING → SHUTTING_DOWN → COMPLETED` re-expressed
/// on vike's fill-driven lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutorState {
    /// Created from an intent; entry not yet submitted.
    Pending,
    /// Entry order in flight / resting (drives refresh + submit-retry).
    EntryWorking,
    /// Position live; barriers armed and evaluated each tick/bar.
    Open,
    /// A barrier fired; the close order is in flight. STICKY — a partial flatten re-submits the
    /// remainder until flat (a position must never be stranded).
    Closing,
    /// Flat. Terminal (records an [`ExecutorOutcome`]).
    Closed,
    /// Entry never landed (retries exhausted). Terminal, no position.
    Failed,
    /// Controller stopped it before any fill. Terminal, no position.
    Canceled,
}

/// Which barrier of the triple fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BarrierKind {
    /// Favourable price target reached (leg 1).
    TakeProfit,
    /// Adverse hard stop crossed (leg 2).
    StopLoss,
    /// Trailing stop crossed (a ratcheting variant of the stop).
    Trailing,
    /// Max holding duration elapsed (leg 3 — the new one).
    Time,
}

/// The result of a barrier check: WHICH barrier fired and the oracle's trigger fill price.
///
/// `exit_px` is the [`vike_model::order_fill_price`] trigger price for a price barrier (gap-adjusted), or
/// the current mark (`bar.close`) for a time exit. It is DIAGNOSTIC — the real close fill is the
/// venue's / the sim engine's when the market close actually resolves (identical contract to
/// `ConditionalBook`'s `FiredConditional::trigger_px`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BarrierHit {
    pub kind: BarrierKind,
    pub exit_px: f64,
}

/// The terminal record an executor produces when it reaches [`ExecutorState::Closed`] (spec §3.1).
/// DATA only — no producer in this stage. `barrier_hit` is `None` for a `Failed`/`Canceled` outcome
/// (no position was ever held).
///
/// ## Realized PnL law (dedup task 7)
/// `realized_pnl` is GROSS — folded through the canonical [`vike_model::TradeFold`] (the same
/// cost-basis primitive `Account`/`SimBroker`/the vector engine/`recon` share), contract-multiplier
/// included ([`PositionExecutor::with_multiplier`]). It matches `Account`'s realized-pnl
/// convention exactly, so `fees` is reported SEPARATELY rather than netted silently into the same
/// field — use [`net`](Self::net) for the fee-adjusted figure.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExecutorOutcome {
    pub barrier_hit: Option<BarrierKind>,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry_px: f64,
    pub exit_px: f64,
    /// gross realized PnL — see the law note above.
    pub realized_pnl: f64,
    /// round-trip fees (entry + close), reported separately (see the law note above).
    pub fees: f64,
}

impl ExecutorOutcome {
    /// Realized PnL net of round-trip fees (`realized_pnl - fees`).
    pub fn net(&self) -> f64 {
        self.realized_pnl - self.fees
    }
}

// --------------------------------------------------------------------------------------------
// Pure barrier-check free functions
// --------------------------------------------------------------------------------------------

/// The absolute take-profit PRICE for a position (`side` +1 long / -1 short) entered at `entry_px`,
/// given a positive price `offset`. Long targets ABOVE (`entry + offset`), short BELOW.
pub fn take_profit_price(entry_px: f64, side: i32, offset: f64) -> f64 {
    if side >= 0 { entry_px + offset } else { entry_px - offset }
}

/// The absolute stop-loss PRICE. Long stops BELOW (`entry - offset`), short ABOVE.
pub fn stop_loss_price(entry_px: f64, side: i32, offset: f64) -> f64 {
    if side >= 0 { entry_px - offset } else { entry_px + offset }
}

/// The NEW time-barrier predicate: `true` once `now` reaches the deadline `entry_ts + time_limit_ms`
/// (fires AT the deadline and after, never before). Pure; `entry_ts`/`now` are injected epoch-ms —
/// no wall clock — so it is deterministic and reproduces in backtest. `saturating_add` guards the
/// (pathological) i64 overflow.
pub fn time_barrier_hit(entry_ts: i64, time_limit_ms: i64, now: i64) -> bool {
    now >= entry_ts.saturating_add(time_limit_ms)
}

/// The exit side of a position (`+1` long / anything-else short): the OPPOSITE side, reduce-only.
#[inline]
fn exit_side(position_side: i32) -> i32 {
    if position_side >= 0 { -1 } else { 1 }
}

/// Evaluate ALL armed barriers of `barriers` for a position (`side`, `entry_px`, `entry_ts`) against
/// the current observation (`now`, `bar`), returning the FIRST barrier hit per the precedence rule
/// below, or `None` if the position survives this bar/tick.
///
/// Each price barrier is detected with [`vike_model::order_fill_price`] (the gap-open oracle): the
/// stop-loss as a reduce-only STOP, the take-profit as a reduce-only LIMIT, the trailing stop as a
/// TRAILING order whose extreme is carried in `trail_extreme` and RATCHETED in place — seeded from
/// `entry_px` on the first check if it is `None` (equivalent to arming the trail off the entry mark,
/// as `ConditionalBook::add_trailing` does). The time barrier uses [`time_barrier_hit`].
///
/// ## Precedence (fixed, deterministic): StopLoss → Trailing → TakeProfit → Time
/// Within one ambiguous bar the intrabar path is unknown, so the ADVERSE (protective) barriers are
/// assumed to hit first — the same no-look-ahead-optimism convention the backtest engines apply to
/// protective stops. Consequently a wide bar that straddles BOTH a stop and the target resolves to
/// the stop (conservative), and a position past its deadline that ALSO crossed a price barrier
/// reports the price barrier (a real cross is more specific — and more pessimistic — than the
/// timeout, so time is always evaluated LAST).
pub fn evaluate_barriers(
    entry_px: f64,
    side: i32,
    entry_ts: i64,
    barriers: &TripleBarrier,
    now: i64,
    bar: &Bar,
    trail_extreme: &mut Option<f64>,
) -> Option<BarrierHit> {
    let exit = exit_side(side);

    // 1. Hard stop-loss — the adverse barrier, checked first (pessimistic).
    if let Some(offset) = barriers.stop_loss {
        let mut o = WorkingOrder::new(OrderKind::Stop, exit, 0.0);
        o.price = Some(stop_loss_price(entry_px, side, offset));
        if let Some(fp) = order_fill_price(&mut o, bar) {
            return Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: fp });
        }
    }

    // 2. Trailing stop — also protective; seed + ratchet the extreme in place.
    if let Some(trail) = barriers.trailing {
        let seed = *trail_extreme.get_or_insert(entry_px);
        let mut o = WorkingOrder::new(OrderKind::Trailing, exit, 0.0);
        o.trail = Some(trail);
        o.extreme = Some(seed);
        let hit = order_fill_price(&mut o, bar);
        // On a non-fire `order_fill_price` ratcheted `o.extreme`; on a fire it left it unchanged.
        // Persist it either way (moot on a fire — the position is closing).
        *trail_extreme = o.extreme;
        if let Some(fp) = hit {
            return Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: fp });
        }
    }

    // 3. Take-profit — the favourable barrier, only after the stops are cleared.
    if let Some(offset) = barriers.take_profit {
        let mut o = WorkingOrder::new(OrderKind::Limit, exit, 0.0);
        o.price = Some(take_profit_price(entry_px, side, offset));
        if let Some(fp) = order_fill_price(&mut o, bar) {
            return Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: fp });
        }
    }

    // 4. Time barrier — the clock catch-all, checked last.
    if let Some(limit) = barriers.time_limit_ms
        && time_barrier_hit(entry_ts, limit, now)
    {
        return Some(BarrierHit { kind: BarrierKind::Time, exit_px: bar.close });
    }

    None
}

/// [`evaluate_barriers`] at a single tick `px` — a degenerate OHLC bar at `px` (open=high=low=close),
/// mirroring `ConditionalBook::check_price`. The convenience for the quote/trade tick path.
#[allow(clippy::too_many_arguments)]
pub fn evaluate_barriers_at_price(
    entry_px: f64,
    side: i32,
    entry_ts: i64,
    barriers: &TripleBarrier,
    now: i64,
    px: f64,
    trail_extreme: &mut Option<f64>,
) -> Option<BarrierHit> {
    evaluate_barriers(
        entry_px,
        side,
        entry_ts,
        barriers,
        now,
        &one_price_bar(now, px),
        trail_extreme,
    )
}

// ============================================================================================
// The PositionExecutor state machine (STAGE 2)
// ============================================================================================

/// Drives ONE position's full lifecycle over a portable [`Broker`] handle:
/// `Pending → EntryWorking → Open → Closing → Closed` (plus the terminal `Failed`/`Canceled`).
///
/// It is a PLAIN struct with explicit step methods — [`start`](Self::start), [`on_bar`](Self::on_bar),
/// [`on_tick`](Self::on_tick), [`on_fill`](Self::on_fill), [`on_order_rejected`](Self::on_order_rejected),
/// [`cancel`](Self::cancel) — that a harness (stage 5) will call from the matching `Strategy` hooks.
/// It is deliberately NOT a `Strategy` itself (that is the harness's job). Every order crosses the
/// SAME `Broker` (so it still passes the live `RiskGate`), and ALL time comes from the injected
/// [`Broker::now`] and the fill's own epoch-ms `ts` — never a wall clock — so the machine is
/// deterministic and will run bit-identically live and in backtest.
///
/// ## Transitions
/// - `Pending → EntryWorking`: [`start`](Self::start) submits the entry per [`EntryKind`] (a market
///   now, or a resting limit).
/// - `EntryWorking → Open`: the entry fills FULLY ([`on_fill`](Self::on_fill) accumulates partials);
///   the VWAP entry price + entry timestamp are recorded and the barriers arm.
/// - `Open → Closing`: the FIRST [`BarrierHit`] from [`evaluate_barriers`] submits a market close.
/// - `Closing → Closed`: the close fills FULLY; the terminal [`ExecutorOutcome`] is produced.
/// - `EntryWorking → Failed`: the entry was rejected/denied ([`on_order_rejected`](Self::on_order_rejected)).
/// - `Pending|EntryWorking → Canceled`: an external stop before any inventory ([`cancel`](Self::cancel)).
///
/// ## Barrier reuse
/// While `Open`, each bar/tick calls the stage-1 [`evaluate_barriers`] (respectively
/// [`evaluate_barriers_at_price`]) with the recorded entry price / entry timestamp and the carried
/// trailing extreme, so the exit semantics are exactly the proven [`vike_model::order_fill_price`] gap-open
/// / trailing-ratchet law — identical to the `ConditionalBook` emulator and the backtest engines.
///
/// ## Sticky close (invariant)
/// Once `Closing`, the executor NEVER re-opens or re-evaluates entry/barriers:
/// [`on_bar`](Self::on_bar)/[`on_tick`](Self::on_tick) are no-ops until the close completes, so a late
/// FAVOURABLE tick can not flip a closing position back to `Open`. A position, once told to flatten,
/// only completes the flatten (a position must never be stranded).
///
/// ## Refresh / retry / close-reject (STAGE 4)
/// [`on_refresh`](Self::on_refresh) is the harness-driven TIMER step: it resubmits a backed-off entry
/// retry that has come due AND reprices a still-resting, wholly-unfilled LIMIT entry once
/// [`RefreshPolicy::every_ms`] has elapsed since the last (re)submit (to the current mark). Because
/// the portable [`Broker`] exposes NEITHER a modify NOR a cancel verb (only `submit_*`), BOTH
/// [`RefreshMode`] variants degrade to one fresh `submit_limit` — the stale resting entry is
/// abandoned at this layer, exactly as [`cancel`](Self::cancel) notes; honoring `Reprice`
/// (modify-in-place) vs `CancelReplace` (cancel-then-submit) needs the richer
/// [`vike_model::HftBroker::modify_tagged`] / [`vike_model::HftBroker::cancel_tagged`] verbs and is a later
/// live-only enhancement (the `mode` is preserved on the intent for it).
/// [`on_order_rejected`](Self::on_order_rejected) (fed from stage 3's order-event lane via
/// [`on_order_event`](Self::on_order_event)) resubmits a rejected ENTRY up to
/// [`RetryPolicy::max_attempts`], spaced by `backoff_ms` (tracked against [`Broker::now`], re-driven
/// by [`on_refresh`](Self::on_refresh)), terminalizing `Failed` only once the budget is exhausted; a
/// rejected CLOSE resubmits the unflattened remainder and STAYS `Closing` (the sticky-close
/// invariant — never re-open).
///
/// ## Deferred to later stages
/// The post-exit COOLDOWN is a CONTROLLER-level "don't re-open `(venue,symbol)` before
/// `last_close_ts + cooldown_ms`" gate (spec §3.2) — it sequences SUCCESSIVE executors, so it belongs
/// to the stage-5 controller/harness, NOT to this single-position executor. The `Controller` /
/// `ControllerHarness` glue (stage 5) and the backtest/live wiring + parity gate (stage 6) also stay
/// out. A close is still a plain opposite-side market on the portable `Broker` (true reduce-only is a
/// later live-hardening verb, not on the common denominator).
#[derive(Debug)]
pub struct PositionExecutor {
    intent: PositionIntent,
    state: ExecutorState,
    /// accumulated entry-fill qty (handles partial entry fills)
    entry_qty: f64,
    /// Σ size·price over entry fills, for the entry VWAP
    entry_notional: f64,
    /// entry VWAP, finalised on the `EntryWorking → Open` transition
    entry_px: f64,
    /// epoch-ms of the fill that COMPLETED the entry — the time barrier's origin
    entry_ts: i64,
    /// accumulated close-fill qty
    exit_qty: f64,
    /// Σ size·price over close fills, for the exit VWAP
    exit_notional: f64,
    /// exit VWAP, finalised on the `Closing → Closed` transition
    exit_px: f64,
    /// epoch-ms of the fill that completed the close
    exit_ts: i64,
    /// round-trip fees accumulated across entry + close fills
    fees: f64,
    /// the canonical cost-basis fold ([`vike_model::TradeFold`]) driving [`ExecutorOutcome::realized_pnl`]
    /// — folds the entry fills (`Open`/`Add`) then the close fills (`Reduce`/`Close`/`Flip`), so a
    /// closed round-trip carries the SAME multiplier-aware, fee-apportioned math `Account`/
    /// `SimBroker`/the vector engine/`recon` share (dedup task 7). Replaces the old hand-rolled
    /// `(exit_px − entry_px) · signed_qty − fees` VWAP-diff.
    fold: TradeFold,
    /// running sum of GROSS realized PnL across the close-fill fold steps — see `fold`. Finalized
    /// into [`ExecutorOutcome::realized_pnl`] on [`finish_closed`](Self::finish_closed).
    realized_pnl: f64,
    /// contract multiplier threaded into `fold` (default `1.0` — unit contracts, inert for every
    /// current caller). NOT sourced from [`PositionIntent`] — the controller layer carries no
    /// instrument metadata today. A stage-6+ harness should resolve it from the venue's fetched
    /// `SymbolProperties` / the `vike_catalog::Instrument` catalog entry and pass it through
    /// [`with_multiplier`](Self::with_multiplier) when it is not `1.0` (e.g. a futures/perp
    /// contract with a per-unit notional multiplier).
    multiplier: f64,
    /// the trailing-stop extreme, seeded from the entry price and ratcheted in place each check
    trail_extreme: Option<f64>,
    /// which barrier fired (`None` for a manual flatten via [`cancel`](Self::cancel))
    barrier_hit: Option<BarrierKind>,
    /// the terminal record, set on reaching `Closed`
    outcome: Option<ExecutorOutcome>,
    /// STAGE 4 — entry submissions made so far (initial [`start`](Self::start) counts as 1); the
    /// [`RetryPolicy`] budget is exhausted once this reaches `max_attempts`.
    entry_attempts: u32,
    /// STAGE 4 — epoch-ms of the last entry (re)submit / reprice — the [`RefreshPolicy`] timer's
    /// origin. Reset on `start`, on each retry resubmit, and on each refresh reprice.
    last_entry_submit_ms: i64,
    /// STAGE 4 — while a rejected entry is backing off, the epoch-ms at/after which the retry
    /// resubmits; `None` when no retry is pending. Stamped `now + backoff_ms` on a retryable reject.
    retry_at_ms: Option<i64>,
}

impl PositionExecutor {
    /// Create a `Pending` executor for `intent` — nothing is submitted until [`start`](Self::start).
    pub fn new(intent: PositionIntent) -> Self {
        PositionExecutor {
            intent,
            state: ExecutorState::Pending,
            entry_qty: 0.0,
            entry_notional: 0.0,
            entry_px: 0.0,
            entry_ts: 0,
            exit_qty: 0.0,
            exit_notional: 0.0,
            exit_px: 0.0,
            exit_ts: 0,
            fees: 0.0,
            fold: TradeFold::default(),
            realized_pnl: 0.0,
            multiplier: 1.0,
            trail_extreme: None,
            barrier_hit: None,
            outcome: None,
            entry_attempts: 0,
            last_entry_submit_ms: 0,
            retry_at_ms: None,
        }
    }

    /// Override the contract multiplier the realized-pnl [`TradeFold`] uses (default `1.0`). Call
    /// before [`start`](Self::start) — e.g. `PositionExecutor::new(intent).with_multiplier(10.0)`.
    /// See the `multiplier` field doc for where this should come from.
    pub fn with_multiplier(mut self, multiplier: f64) -> Self {
        self.multiplier = multiplier;
        self
    }

    /// The current lifecycle state.
    pub fn state(&self) -> ExecutorState {
        self.state
    }

    /// The intent this executor is driving.
    pub fn intent(&self) -> &PositionIntent {
        &self.intent
    }

    /// The terminal outcome — `Some` once the position round-trip reached `Closed`; `None` while live
    /// and for the `Failed`/`Canceled` terminals (no position was ever held).
    pub fn outcome(&self) -> Option<ExecutorOutcome> {
        self.outcome
    }

    /// `true` once the executor reached a terminal state (`Closed`/`Failed`/`Canceled`).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            ExecutorState::Closed | ExecutorState::Failed | ExecutorState::Canceled
        )
    }

    /// Submit the entry per [`PositionIntent::entry`] and move `Pending → EntryWorking`. Idempotent —
    /// a call in any non-`Pending` state is a no-op.
    pub fn start<B: Broker>(&mut self, broker: &mut B) {
        if self.state != ExecutorState::Pending {
            return;
        }
        let now = broker.now();
        self.submit_entry(broker, now);
        self.state = ExecutorState::EntryWorking;
    }

    /// Evaluate the barriers against a closed `bar` while `Open` (a no-op in every other state — the
    /// sticky-close guard). On the first [`BarrierHit`] submit the market close and move to `Closing`.
    pub fn on_bar<B: Broker>(&mut self, broker: &mut B, bar: &Bar) {
        if self.state != ExecutorState::Open {
            return;
        }
        let now = broker.now();
        if let Some(hit) = evaluate_barriers(
            self.entry_px,
            self.intent.side,
            self.entry_ts,
            &self.intent.barriers,
            now,
            bar,
            &mut self.trail_extreme,
        ) {
            self.begin_close(broker, Some(hit.kind));
        }
    }

    /// Evaluate the barriers against a single tick price `px` while `Open` (a no-op otherwise) — the
    /// tick twin of [`on_bar`](Self::on_bar), a degenerate one-price bar mirroring
    /// `ConditionalBook::check_price`.
    pub fn on_tick<B: Broker>(&mut self, broker: &mut B, px: f64) {
        if self.state != ExecutorState::Open {
            return;
        }
        let now = broker.now();
        if let Some(hit) = evaluate_barriers_at_price(
            self.entry_px,
            self.intent.side,
            self.entry_ts,
            &self.intent.barriers,
            now,
            px,
            &mut self.trail_extreme,
        ) {
            self.begin_close(broker, Some(hit.kind));
        }
    }

    /// The harness-driven TIMER step (spec §3.2: evaluated on the per-tick/per-bar cadence against
    /// the injected [`Broker::now`], NEVER a wall clock — so it reproduces in backtest). It does two
    /// time-based jobs and is a no-op in every state but `EntryWorking`:
    /// 1. **Retry** — resubmit a backed-off entry retry (scheduled by
    ///    [`on_order_rejected`](Self::on_order_rejected)) once its `backoff_ms` deadline is reached.
    /// 2. **Refresh** — if the entry is a still-resting, wholly-unfilled LIMIT and its
    ///    [`RefreshPolicy::every_ms`] has elapsed since the last (re)submit, RE-PRICE it to the
    ///    current mark ([`Broker::price`]). A no-op once the position is `Open` (nothing to reprice),
    ///    for a market entry, while a retry is pending, or with no refresh policy armed.
    ///
    /// NOTE (portable-broker limitation): the common [`Broker`] surface has NEITHER a modify NOR a
    /// cancel verb (only `submit_*`), so BOTH [`RefreshMode`] variants take the SAME action here — a
    /// fresh `submit_limit` at the new price; the stale resting order is abandoned at this layer
    /// (exactly as [`cancel`](Self::cancel) documents). Honoring `Reprice` (modify-in-place, keep
    /// queue priority) vs `CancelReplace` (cancel-then-submit) needs the richer [`vike_model::HftBroker`]
    /// verbs and is a later live-only enhancement; the `mode` is preserved on the intent for it.
    pub fn on_refresh<B: Broker>(&mut self, broker: &mut B) {
        let now = broker.now();
        // (1) a backed-off entry retry that has become due.
        self.drive_entry_retry(broker, now);
        // (2) reprice a resting, unfilled LIMIT entry whose refresh interval elapsed.
        if self.state != ExecutorState::EntryWorking {
            return;
        }
        if self.retry_at_ms.is_some() || self.entry_qty > 0.0 {
            // awaiting a retry (nothing is resting) or partially filled (leave the remainder alone).
            return;
        }
        if !matches!(self.intent.entry, EntryKind::Limit { .. }) {
            return; // a market entry has no resting order to reprice.
        }
        let Some(policy) = self.intent.refresh else {
            return; // no refresh policy armed.
        };
        if now < self.last_entry_submit_ms.saturating_add(policy.every_ms) {
            return; // the refresh interval has not elapsed.
        }
        let new_px = broker.price(&self.intent.symbol);
        broker.submit_limit(&self.intent.symbol, self.intent.side, self.intent.qty, new_px);
        self.last_entry_submit_ms = now;
    }

    /// Fold one execution into the lifecycle. The STATE disambiguates it: an `EntryWorking` fill is an
    /// entry fill (accumulated; a FULL fill opens the position), a `Closing` fill is a close fill
    /// (accumulated; a full flatten produces the terminal [`ExecutorOutcome`]). Fills in any other
    /// state are ignored (defensive — nothing is in flight).
    pub fn on_fill(&mut self, fill: &Fill) {
        match self.state {
            ExecutorState::EntryWorking => self.apply_entry_fill(fill),
            ExecutorState::Closing => self.apply_close_fill(fill),
            _ => {}
        }
    }

    /// Route a stage-3 [`OrderLifecycle`] to the executor's reaction — the single entry point a
    /// stage-5 harness calls from its own
    /// [`Strategy::on_order_event`](vike_model::Strategy::on_order_event). A `Rejected` / `Denied`
    /// transition is a retryable reject (forwarded to [`on_order_rejected`](Self::on_order_rejected));
    /// the venue's own `Accepted` / `Canceled` / `Expired` are no-ops here in stage 4 (a resting-limit
    /// refresh keys off the submit timer, not an `Accepted` ack; reacting to a venue cancel/expire of
    /// our own order is a stage-5+ controller concern).
    ///
    /// [`OrderEventKind::Filled`] is a no-op here for a reason that is NOT "stage 5": the executor's
    /// whole fill accounting runs off [`on_fill`](Self::on_fill), which fires first and carries the
    /// quantity — the completion notice adds nothing this state machine does not already know, and
    /// acting on it would double-drive the entry/close transitions.
    pub fn on_order_event<B: Broker>(&mut self, broker: &mut B, event: &OrderLifecycle) {
        match event.kind {
            OrderEventKind::Rejected { .. } | OrderEventKind::Denied { .. } => {
                self.on_order_rejected(broker)
            }
            OrderEventKind::Accepted
            | OrderEventKind::Canceled { .. }
            | OrderEventKind::Expired
            | OrderEventKind::Filled => {}
        }
    }

    /// React to a REJECTED / DENIED order (fed from the stage-3 order-event lane, typically via
    /// [`on_order_event`](Self::on_order_event)). Both a venue `Rejected` and a `RiskGate` `Denied`
    /// are treated identically — the order never established, so it is retryable.
    ///
    /// - **Entry reject** (while `EntryWorking`, nothing filled yet): if the [`RetryPolicy`] budget
    ///   remains (`attempts < max_attempts`), schedule a resubmit `backoff_ms` in the future (measured
    ///   from [`Broker::now`]) and fire it immediately if already due (a `0` backoff); once the budget
    ///   is exhausted terminalize as `Failed`. Any backed-off resubmit is re-driven by
    ///   [`on_refresh`](Self::on_refresh).
    /// - **Close reject** (while `Closing`): resubmit the still-unflattened remainder and STAY
    ///   `Closing` — the sticky-close invariant (a position told to flatten must never be stranded or
    ///   re-opened).
    ///
    /// A reject in any other state, or of a PARTIALLY-filled entry, is ignored (defensive — there is
    /// no clean single order to retry).
    pub fn on_order_rejected<B: Broker>(&mut self, broker: &mut B) {
        let now = broker.now();
        match self.state {
            ExecutorState::EntryWorking => {
                if self.entry_qty > 0.0 {
                    // a partially-filled entry's remainder was rejected — don't spawn a duplicate
                    // full-size retry; leave it working for the harness/controller to resolve.
                    return;
                }
                if self.entry_attempts < self.intent.retry.max_attempts {
                    self.retry_at_ms = Some(now.saturating_add(self.intent.retry.backoff_ms));
                    self.drive_entry_retry(broker, now); // fire now if the backoff is already due
                } else {
                    self.state = ExecutorState::Failed;
                }
            }
            ExecutorState::Closing => {
                // sticky close: re-submit the remaining flatten, never re-open.
                let remaining = self.entry_qty - self.exit_qty;
                if remaining > 0.0 {
                    let exit = exit_side(self.intent.side);
                    broker.submit_market(&self.intent.symbol, exit, remaining);
                }
            }
            _ => {}
        }
    }

    /// External stop (the controller pulled this executor). Before any inventory exists it
    /// terminalizes as `Canceled`; if the position is already (partly) open it FLATTENS instead
    /// (submits the market close → `Closing`) so a stop never strands inventory. A no-op once already
    /// `Closing`/terminal.
    pub fn cancel<B: Broker>(&mut self, broker: &mut B) {
        match self.state {
            ExecutorState::Pending => self.state = ExecutorState::Canceled,
            ExecutorState::EntryWorking => {
                if self.entry_qty > 0.0 {
                    // a partial entry already left inventory — flatten it, don't strand it
                    self.begin_close(broker, None);
                } else {
                    // NOTE: the portable `Broker` has no cancel verb, so a resting limit entry is
                    // simply abandoned here; cancelling it at the venue is a richer-broker / stage-4
                    // concern.
                    self.state = ExecutorState::Canceled;
                }
            }
            ExecutorState::Open => self.begin_close(broker, None),
            _ => {}
        }
    }

    // ---- internals ----

    /// Submit the entry per [`PositionIntent::entry`], counting the attempt and stamping the refresh
    /// timer's origin (`now`). Shared by [`start`](Self::start) and the retry resubmit.
    fn submit_entry<B: Broker>(&mut self, broker: &mut B, now: i64) {
        let side = self.intent.side;
        let qty = self.intent.qty;
        match self.intent.entry {
            EntryKind::Market => broker.submit_market(&self.intent.symbol, side, qty),
            EntryKind::Limit { price } => {
                broker.submit_limit(&self.intent.symbol, side, qty, price)
            }
        }
        self.entry_attempts += 1;
        self.last_entry_submit_ms = now;
    }

    /// Resubmit a pending entry retry once its backoff deadline `retry_at_ms` is reached — only while
    /// `EntryWorking` (a fill/terminal in the meantime cancels it). Clears the pending mark and
    /// re-stamps the refresh timer via [`submit_entry`](Self::submit_entry).
    fn drive_entry_retry<B: Broker>(&mut self, broker: &mut B, now: i64) {
        if self.state != ExecutorState::EntryWorking {
            return;
        }
        if let Some(due) = self.retry_at_ms
            && now >= due
        {
            self.retry_at_ms = None;
            self.submit_entry(broker, now);
        }
    }

    /// Submit the reduce (opposite-side) market close for the tracked position qty and enter the
    /// sticky `Closing` state. `kind` is the firing barrier (`None` for a manual flatten).
    fn begin_close<B: Broker>(&mut self, broker: &mut B, kind: Option<BarrierKind>) {
        let exit = exit_side(self.intent.side);
        broker.submit_market(&self.intent.symbol, exit, self.entry_qty);
        self.barrier_hit = kind;
        self.state = ExecutorState::Closing;
    }

    fn apply_entry_fill(&mut self, fill: &Fill) {
        self.entry_qty += fill.size;
        self.entry_notional += fill.size * fill.price;
        self.fees += fill.fee;
        self.fold.apply(
            self.intent.side,
            fill.size,
            fill.price,
            fill.fee,
            fill.ts,
            self.multiplier,
        );
        // The position OPENS on the FULL entry fill (spec diagram: EntryWorking → Open on `full`).
        if self.entry_qty >= self.intent.qty {
            self.entry_px = self.entry_notional / self.entry_qty;
            self.entry_ts = fill.ts;
            self.retry_at_ms = None; // opened — drop any pending entry retry
            self.state = ExecutorState::Open;
        }
    }

    fn apply_close_fill(&mut self, fill: &Fill) {
        self.exit_qty += fill.size;
        self.exit_notional += fill.size * fill.price;
        self.fees += fill.fee;

        // Fold this close fill through the canonical `TradeFold` law (contract multiplier
        // included), clamped to the remaining unclosed qty. The executor's contract is
        // close-to-flat: an overshoot (this fill's qty, plus what already closed, summing PAST
        // the tracked entry qty — a reduce-only close should never overfill, but a venue/
        // accounting anomaly is not impossible) clamps AT flat instead of folding a stray
        // opposite-side flip this executor would then abandon untracked.
        let already_closed = self.exit_qty - fill.size;
        let remaining = (self.entry_qty - already_closed).max(0.0);
        let closing_qty = fill.size.min(remaining);
        if closing_qty > 0.0 {
            let step = self.fold.apply(
                exit_side(self.intent.side),
                closing_qty,
                fill.price,
                fill.fee,
                fill.ts,
                self.multiplier,
            );
            if let Some(closed) = step.closed {
                self.realized_pnl += closed.pnl;
            }
        }

        // Flat once the close has covered the tracked entry qty.
        if self.exit_qty >= self.entry_qty {
            self.exit_px = self.exit_notional / self.exit_qty;
            self.exit_ts = fill.ts;
            self.finish_closed();
        }
    }

    fn finish_closed(&mut self) {
        self.outcome = Some(ExecutorOutcome {
            barrier_hit: self.barrier_hit,
            entry_ts: self.entry_ts,
            exit_ts: self.exit_ts,
            entry_px: self.entry_px,
            exit_px: self.exit_px,
            realized_pnl: self.realized_pnl,
            fees: self.fees,
        });
        self.state = ExecutorState::Closed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build an OHLC bar (ts unused by the price barriers).
    fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            ts: 0,
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

    // --- price helpers (golden) ---

    #[test]
    fn barrier_prices_long_and_short() {
        // long entered at 100: TP is +10 above, SL is -5 below
        assert_eq!(take_profit_price(100.0, 1, 10.0), 110.0);
        assert_eq!(stop_loss_price(100.0, 1, 5.0), 95.0);
        // short entered at 100: TP is -10 below, SL is +5 above
        assert_eq!(take_profit_price(100.0, -1, 10.0), 90.0);
        assert_eq!(stop_loss_price(100.0, -1, 5.0), 105.0);
    }

    // --- take-profit ---

    #[test]
    fn tp_hit_long() {
        let b = TripleBarrier::new(Some(10.0), None, None, None); // tp @ 110
        let mut ext = None;
        // bar rallies through 110
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(105.0, 112.0, 104.0, 111.0), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 110.0 }));
    }

    #[test]
    fn tp_hit_short() {
        let b = TripleBarrier::new(Some(10.0), None, None, None); // tp @ 90
        let mut ext = None;
        // bar dips through 90
        let hit = evaluate_barriers(100.0, -1, 0, &b, 0, &bar(95.0, 96.0, 88.0, 89.0), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 90.0 }));
    }

    #[test]
    fn tp_gap_open_improves_fill_long() {
        // gap UP through the 110 target: a favourable limit fills at the better open (113), not 110
        let b = TripleBarrier::new(Some(10.0), None, None, None);
        let mut ext = None;
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(113.0, 114.0, 112.0, 113.5), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 113.0 }));
    }

    // --- stop-loss ---

    #[test]
    fn sl_hit_long() {
        let b = TripleBarrier::new(None, Some(5.0), None, None); // sl @ 95
        let mut ext = None;
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(98.0, 99.0, 94.0, 94.5), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 95.0 }));
    }

    #[test]
    fn sl_hit_short() {
        let b = TripleBarrier::new(None, Some(5.0), None, None); // sl @ 105
        let mut ext = None;
        let hit =
            evaluate_barriers(100.0, -1, 0, &b, 0, &bar(102.0, 106.0, 101.0, 105.5), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 105.0 }));
    }

    #[test]
    fn sl_gap_open_worsens_fill_long() {
        // gap DOWN through the 95 stop: an adverse stop fills at the worse open (92), not 95
        // (mirrors ConditionalBook::gap_open_fills_adverse)
        let b = TripleBarrier::new(None, Some(5.0), None, None);
        let mut ext = None;
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(92.0, 93.0, 91.0, 92.5), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 92.0 }));
    }

    // --- time barrier (the new leg) ---

    #[test]
    fn time_barrier_hit_boundary() {
        // deadline = 1000 + 500 = 1500: not before, fires AT and after
        assert!(!time_barrier_hit(1000, 500, 1499));
        assert!(time_barrier_hit(1000, 500, 1500));
        assert!(time_barrier_hit(1000, 500, 1600));
    }

    #[test]
    fn time_barrier_via_evaluate_fires_at_deadline() {
        let b = TripleBarrier::new(None, None, Some(500), None); // deadline entry_ts+500
        let mut ext = None;
        let quiet = bar(100.0, 101.0, 99.0, 100.5); // no price barrier could fire (none armed)
        // before the deadline: nothing
        assert_eq!(evaluate_barriers(100.0, 1, 1000, &b, 1499, &quiet, &mut ext), None);
        // at the deadline: Time fires, exit_px = current mark (bar.close)
        assert_eq!(
            evaluate_barriers(100.0, 1, 1000, &b, 1500, &quiet, &mut ext),
            Some(BarrierHit { kind: BarrierKind::Time, exit_px: 100.5 }),
        );
    }

    // --- none-hit ---

    #[test]
    fn none_hit_within_range() {
        let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None); // tp 110, sl 95
        let mut ext = None;
        // bar stays between 95 and 110
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(100.0, 108.0, 96.0, 102.0), &mut ext);
        assert_eq!(hit, None);
    }

    // --- precedence ---

    #[test]
    fn precedence_stop_before_target_on_a_wide_bar() {
        // GOLDEN anchor: a bar straddling BOTH the 95 stop and the 110 target must resolve to the
        // STOP (assume the adverse leg hit first — no intrabar look-ahead optimism).
        let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None); // tp 110, sl 95
        let mut ext = None;
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(100.0, 115.0, 90.0, 112.0), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 95.0 }));
    }

    #[test]
    fn precedence_price_barrier_before_time() {
        // past the deadline AND the bar crosses the stop → report the price barrier, not the timeout.
        let b = TripleBarrier::new(None, Some(5.0), Some(500), None); // sl 95, deadline entry_ts+500
        let mut ext = None;
        let hit =
            evaluate_barriers(100.0, 1, 1000, &b, 1500, &bar(98.0, 99.0, 94.0, 94.5), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 95.0 }));
    }

    // --- trailing (ratchet parity vs ConditionalBook) ---

    #[test]
    fn trailing_ratchets_in_place_then_fires_long() {
        // Replicates vike-core ConditionalBook::trailing_ratchets_in_place_then_fires exactly:
        // long protection, trail 5, extreme seeded 100 → trigger 95.
        let b = TripleBarrier::new(None, None, None, Some(5.0));
        let mut ext = Some(100.0);
        // new high 110 ratchets the extreme; low 101 stays above the OLD trigger 95 → no fire
        assert_eq!(
            evaluate_barriers(100.0, 1, 0, &b, 0, &bar(105.0, 110.0, 101.0, 108.0), &mut ext),
            None,
        );
        assert_eq!(ext, Some(110.0)); // extreme ratcheted
        // trigger is now 105: a dip to 104 fires at 105 (GOLDEN — identical to ConditionalBook)
        assert_eq!(
            evaluate_barriers(100.0, 1, 0, &b, 0, &bar(106.0, 107.0, 104.0, 104.5), &mut ext),
            Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: 105.0 }),
        );
    }

    #[test]
    fn trailing_new_high_bar_cannot_stop_itself_out() {
        // the oracle checks the PRIOR extreme's trigger before ratcheting (mirrors ConditionalBook)
        let b = TripleBarrier::new(None, None, None, Some(5.0));
        let mut ext = Some(100.0);
        // high 120 would imply trigger 115 — but the low 103 is compared to the PRIOR trigger 95: no fire
        assert_eq!(
            evaluate_barriers(100.0, 1, 0, &b, 0, &bar(110.0, 120.0, 103.0, 118.0), &mut ext),
            None,
        );
        assert_eq!(ext, Some(120.0));
    }

    #[test]
    fn trailing_lazy_seeds_from_entry_px() {
        // passing &mut None seeds the extreme from entry_px (100) → trigger 95; a dip to 94 fires at 95
        let b = TripleBarrier::new(None, None, None, Some(5.0));
        let mut ext = None;
        let hit = evaluate_barriers(100.0, 1, 0, &b, 0, &bar(96.0, 97.0, 94.0, 94.5), &mut ext);
        assert_eq!(hit, Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: 95.0 }));
    }

    #[test]
    fn trailing_short_ratchets_down_then_fires() {
        // short protection: buy-stop trailing the LOW. trail 5, extreme seeded 100 → trigger 105.
        let b = TripleBarrier::new(None, None, None, Some(5.0));
        let mut ext = Some(100.0);
        // low 90 ratchets the extreme down; high 96 stays below the OLD trigger 105 → no fire
        assert_eq!(
            evaluate_barriers(100.0, -1, 0, &b, 0, &bar(95.0, 96.0, 90.0, 94.0), &mut ext),
            None,
        );
        assert_eq!(ext, Some(90.0)); // extreme ratcheted DOWN
        // trigger is now 95: a rally to 96 fires at 95
        assert_eq!(
            evaluate_barriers(100.0, -1, 0, &b, 0, &bar(94.0, 96.0, 93.0, 95.0), &mut ext),
            Some(BarrierHit { kind: BarrierKind::Trailing, exit_px: 95.0 }),
        );
    }

    // --- tick convenience ---

    #[test]
    fn evaluate_at_price_tick_path() {
        // tp 110, sl 95
        let b = TripleBarrier::new(Some(10.0), Some(5.0), None, None);
        // a tick exactly AT the 110 take-profit fills there
        let mut ext = None;
        assert_eq!(
            evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 110.0, &mut ext),
            Some(BarrierHit { kind: BarrierKind::TakeProfit, exit_px: 110.0 }),
        );
        // a tick BELOW the 95 stop fills at the tick itself — on a degenerate (single-price) bar the
        // crossing price IS the fill (same as ConditionalBook::check_price: min(95, open=94) = 94)
        let mut ext2 = None;
        assert_eq!(
            evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 94.0, &mut ext2),
            Some(BarrierHit { kind: BarrierKind::StopLoss, exit_px: 94.0 }),
        );
        // a tick between the barriers: nothing
        let mut ext3 = None;
        assert_eq!(evaluate_barriers_at_price(100.0, 1, 0, &b, 0, 100.0, &mut ext3), None);
    }

    // --- from_bps convenience (golden) ---

    #[test]
    fn from_bps_converts_against_ref_px() {
        // ref 100: 100bps = 1% → 1.0 offset; 50bps → 0.5; 25bps → 0.25 trailing
        let b = TripleBarrier::from_bps(100.0, Some(100.0), Some(50.0), Some(500), Some(25.0));
        assert_eq!(b.take_profit, Some(1.0));
        assert_eq!(b.stop_loss, Some(0.5));
        assert_eq!(b.time_limit_ms, Some(500));
        assert_eq!(b.trailing, Some(0.25));
        // a None bps leg stays None
        let b2 = TripleBarrier::from_bps(100.0, None, Some(50.0), None, None);
        assert_eq!(b2.take_profit, None);
        assert_eq!(b2.stop_loss, Some(0.5));
        assert_eq!(b2.time_limit_ms, None);
        assert_eq!(b2.trailing, None);
    }

    // --- defaults ---

    #[test]
    fn defaults_are_unarmed_and_no_retry() {
        assert_eq!(TripleBarrier::default(), TripleBarrier::none());
        assert_eq!(TripleBarrier::default().take_profit, None);
        assert_eq!(RetryPolicy::default(), RetryPolicy { max_attempts: 1, backoff_ms: 0 });
        assert_eq!(EntryKind::default(), EntryKind::Market);
        assert_eq!(RefreshMode::default(), RefreshMode::Reprice);
        // PositionIntent::default is a usable builder base
        let pi = PositionIntent::default();
        assert_eq!(pi.entry, EntryKind::Market);
        assert_eq!(pi.retry, RetryPolicy::default());
        assert_eq!(pi.refresh, None);
    }

    // --- serde (pure, additive) ---

    #[test]
    fn position_intent_round_trips() {
        let pi = PositionIntent {
            venue: "binance".into(),
            symbol: "BTCUSDT".into(),
            side: 1,
            qty: 0.5,
            entry: EntryKind::Limit { price: 65_000.0 },
            barriers: TripleBarrier::new(Some(500.0), Some(250.0), Some(60_000), Some(100.0)),
            refresh: Some(RefreshPolicy::new(5_000, RefreshMode::CancelReplace)),
            retry: RetryPolicy::new(3, 1_000),
        };
        let json = serde_json::to_string(&pi).unwrap();
        let back: PositionIntent = serde_json::from_str(&json).unwrap();
        assert_eq!(pi, back);
    }

    #[test]
    fn position_intent_minimal_json_is_additive() {
        // only the required identity fields — policy fields fall back to defaults
        let pi: PositionIntent =
            serde_json::from_str(r#"{"venue":"binance","symbol":"BTCUSDT","side":-1,"qty":1.0}"#)
                .unwrap();
        assert_eq!(pi.entry, EntryKind::Market);
        assert_eq!(pi.barriers, TripleBarrier::none());
        assert_eq!(pi.refresh, None);
        assert_eq!(pi.retry, RetryPolicy::default());
    }

    #[test]
    fn entry_kind_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&EntryKind::Market).unwrap(), "\"market\"");
        assert_eq!(
            serde_json::to_string(&EntryKind::Limit { price: 100.0 }).unwrap(),
            r#"{"limit":{"price":100.0}}"#,
        );
    }

    #[test]
    fn executor_state_and_barrier_kind_serialize_snake_case() {
        assert_eq!(
            serde_json::to_string(&ExecutorState::EntryWorking).unwrap(),
            "\"entry_working\"",
        );
        assert_eq!(serde_json::to_string(&ExecutorState::Closing).unwrap(), "\"closing\"");
        assert_eq!(serde_json::to_string(&BarrierKind::TakeProfit).unwrap(), "\"take_profit\"");
        assert_eq!(serde_json::to_string(&BarrierKind::Time).unwrap(), "\"time\"");
    }

    #[test]
    fn triple_barrier_partial_json_is_additive() {
        // only take_profit present → the other legs default to None
        let b: TripleBarrier = serde_json::from_str(r#"{"take_profit":10.0}"#).unwrap();
        assert_eq!(b, TripleBarrier::new(Some(10.0), None, None, None));
    }

    #[test]
    fn executor_outcome_round_trips() {
        let o = ExecutorOutcome {
            barrier_hit: Some(BarrierKind::Time),
            entry_ts: 1_000,
            exit_ts: 61_000,
            entry_px: 100.0,
            exit_px: 100.5,
            realized_pnl: 0.5,
            fees: 0.1,
        };
        let json = serde_json::to_string(&o).unwrap();
        assert_eq!(o, serde_json::from_str::<ExecutorOutcome>(&json).unwrap());
        assert_eq!(o.net(), 0.4);
    }
}

// --------------------------------------------------------------------------------------------
// State-machine tests (STAGE 2) — the PositionExecutor lifecycle over a recording mock Broker.
// --------------------------------------------------------------------------------------------
#[cfg(test)]
mod machine_tests {
    use super::*;
    // The recording `Broker` test double is shared from vike-model (behind its `test-support`
    // feature): the executor is proven generic over the `Broker` TRAIT, driven by a mock that
    // captures every market/limit submit and scripts `now`/`price`/`position` — the rest inert.
    use vike_model::strategy::MockBroker;

    fn bar(open: f64, high: f64, low: f64, close: f64) -> Bar {
        Bar {
            ts: 0,
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

    fn fill(side: i32, size: f64, price: f64, ts: i64) -> Fill {
        Fill { side, size, price, fee: 0.0, ts, is_maker: false, symbol: String::new() }
    }

    /// A 1-unit long BTCUSDT market intent with take-profit +10 (target 110 off a 100 entry).
    fn long_tp() -> PositionIntent {
        PositionIntent::market(
            "binance",
            "BTCUSDT",
            1,
            1.0,
            TripleBarrier::new(Some(10.0), None, None, None),
        )
    }

    // --- entry submission (market / limit) ---

    #[test]
    fn market_entry_submits_market_then_opens_on_full_fill() {
        let mut ex = PositionExecutor::new(long_tp());
        let mut b = MockBroker::default();
        assert_eq!(ex.state(), ExecutorState::Pending);
        ex.start(&mut b);
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
        assert_eq!(b.markets, vec![("BTCUSDT".to_string(), 1, 1.0)]);
        assert!(b.limits.is_empty());
        // full entry fill @100 → Open
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
        assert_eq!(ex.state(), ExecutorState::Open);
    }

    #[test]
    fn limit_entry_submits_limit() {
        let intent = PositionIntent { entry: EntryKind::Limit { price: 99.0 }, ..long_tp() };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
        assert!(b.markets.is_empty());
        assert_eq!(b.limits, vec![("BTCUSDT".to_string(), 1, 1.0, 99.0)]);
    }

    #[test]
    fn start_is_idempotent() {
        let mut ex = PositionExecutor::new(long_tp());
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.start(&mut b); // second call ignored (not Pending)
        assert_eq!(b.markets.len(), 1);
    }

    // --- full lifecycle, long & short ---

    #[test]
    fn full_lifecycle_long_take_profit() {
        let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
        assert_eq!(ex.state(), ExecutorState::Open);
        // a bar rallying through 110 fires TP → market close, Closing
        ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0));
        assert_eq!(ex.state(), ExecutorState::Closing);
        assert_eq!(
            b.markets,
            vec![
                ("BTCUSDT".to_string(), 1, 1.0),  // entry (buy)
                ("BTCUSDT".to_string(), -1, 1.0), // close (sell, opposite side)
            ]
        );
        // close fill @110 → Closed with the terminal outcome
        ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert!(ex.is_terminal());
        let o = ex.outcome().expect("a Closed outcome");
        assert_eq!(o.barrier_hit, Some(BarrierKind::TakeProfit));
        assert_eq!(o.entry_px, 100.0);
        assert_eq!(o.exit_px, 110.0);
        assert_eq!(o.entry_ts, 1_000);
        assert_eq!(o.exit_ts, 2_000);
        assert_eq!(o.realized_pnl, 10.0); // (110-100)*(+1)*1 - 0
    }

    #[test]
    fn full_lifecycle_short_take_profit() {
        // short: entry SELLS, tp exits BELOW entry (100 → 90), close BUYS to cover
        let intent = PositionIntent::market(
            "binance",
            "BTCUSDT",
            -1,
            1.0,
            TripleBarrier::new(Some(10.0), None, None, None),
        );
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        assert_eq!(b.markets[0], ("BTCUSDT".to_string(), -1, 1.0)); // short entry sells
        ex.on_fill(&fill(-1, 1.0, 100.0, 1_000)); // Open short @100
        assert_eq!(ex.state(), ExecutorState::Open);
        ex.on_bar(&mut b, &bar(95.0, 96.0, 88.0, 89.0)); // dips through 90 → TP
        assert_eq!(ex.state(), ExecutorState::Closing);
        assert_eq!(b.markets[1], ("BTCUSDT".to_string(), 1, 1.0)); // close buys to cover
        ex.on_fill(&fill(1, 1.0, 90.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed);
        let o = ex.outcome().unwrap();
        assert_eq!(o.barrier_hit, Some(BarrierKind::TakeProfit));
        assert_eq!(o.realized_pnl, 10.0); // (90-100)*(-1)*1 = +10 (short profits as price falls)
    }

    // --- each barrier drives a close ---

    #[test]
    fn stop_loss_drives_close() {
        let intent = PositionIntent::market(
            "binance",
            "BTCUSDT",
            1,
            1.0,
            TripleBarrier::new(None, Some(5.0), None, None), // sl @ 95
        );
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
        ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // dips through 95
        assert_eq!(ex.state(), ExecutorState::Closing);
        ex.on_fill(&fill(-1, 1.0, 95.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
    }

    #[test]
    fn take_profit_via_tick_path_drives_close() {
        // exercises the on_tick cadence for a price barrier
        let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
        ex.on_tick(&mut b, 110.0); // a tick AT the target
        assert_eq!(ex.state(), ExecutorState::Closing);
        ex.on_fill(&fill(-1, 1.0, 110.0, 1_100));
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::TakeProfit));
    }

    #[test]
    fn time_barrier_drives_close() {
        let intent = PositionIntent::market(
            "binance",
            "BTCUSDT",
            1,
            1.0,
            TripleBarrier::new(None, None, Some(500), None), // deadline = entry_ts + 500
        );
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // entry_ts = 1000 → deadline 1500
        // a quiet bar BEFORE the deadline: nothing fires (injected clock = broker.now())
        b.now = 1_499;
        ex.on_bar(&mut b, &bar(100.0, 101.0, 99.0, 100.5));
        assert_eq!(ex.state(), ExecutorState::Open);
        // AT the deadline: Time fires → market close
        b.now = 1_500;
        ex.on_bar(&mut b, &bar(100.0, 101.0, 99.0, 100.5));
        assert_eq!(ex.state(), ExecutorState::Closing);
        ex.on_fill(&fill(-1, 1.0, 100.5, 1_500));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::Time));
    }

    // --- the sticky-close invariant ---

    #[test]
    fn sticky_close_survives_favorable_tick() {
        // long, tp 110 / sl 95. The stop fires first → Closing. Then price rallies WELL past the
        // take-profit via both a tick and a bar — the executor must NOT re-open or submit anything
        // new; it stays Closing until the close fill, and the recorded barrier is the ORIGINAL stop.
        let intent = PositionIntent::market(
            "binance",
            "BTCUSDT",
            1,
            1.0,
            TripleBarrier::new(Some(10.0), Some(5.0), None, None),
        );
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
        ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // SL fires
        assert_eq!(ex.state(), ExecutorState::Closing);
        assert_eq!(b.markets.len(), 2, "entry + one close only");
        // a favourable tick far above entry — would be a TP if still Open — must be IGNORED
        ex.on_tick(&mut b, 120.0);
        // and a favourable bar too
        ex.on_bar(&mut b, &bar(118.0, 125.0, 117.0, 121.0));
        assert_eq!(
            ex.state(),
            ExecutorState::Closing,
            "sticky: never re-opens on a favourable move"
        );
        assert_eq!(b.markets.len(), 2, "no new orders from the favourable ticks/bars");
        // the close finally fills → Closed, still tagged the original stop
        ex.on_fill(&fill(-1, 1.0, 95.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
    }

    // --- failure & cancel paths ---

    #[test]
    fn entry_reject_fails() {
        // long_tp uses the DEFAULT RetryPolicy (max_attempts = 1): the first submit is the only one,
        // so a single reject exhausts the budget → Failed (the stage-2 no-retry behaviour, preserved).
        let mut ex = PositionExecutor::new(long_tp());
        let mut b = MockBroker::default();
        ex.start(&mut b);
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
        ex.on_order_rejected(&mut b);
        assert_eq!(ex.state(), ExecutorState::Failed);
        assert!(ex.is_terminal());
        assert_eq!(ex.outcome(), None); // no position was ever held
        assert_eq!(b.markets.len(), 1, "no resubmit under the default no-retry policy");
    }

    #[test]
    fn external_cancel_before_fill_cancels() {
        let mut ex = PositionExecutor::new(long_tp());
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.cancel(&mut b);
        assert_eq!(ex.state(), ExecutorState::Canceled);
        assert!(ex.is_terminal());
        assert_eq!(b.markets.len(), 1, "no close submitted — nothing was filled");
        assert_eq!(ex.outcome(), None);
    }

    #[test]
    fn cancel_while_open_flattens_not_strands() {
        // a controller stop on an OPEN position must flatten (never strand inventory), not Cancel.
        let mut ex = PositionExecutor::new(long_tp());
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open
        ex.cancel(&mut b);
        assert_eq!(ex.state(), ExecutorState::Closing);
        assert_eq!(b.markets.last().unwrap(), &("BTCUSDT".to_string(), -1, 1.0));
        ex.on_fill(&fill(-1, 1.0, 101.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, None); // a manual flatten has no barrier
    }

    // --- partial entry accumulation + VWAP ---

    #[test]
    fn partial_entry_fills_accumulate_then_open_at_vwap() {
        let intent = PositionIntent::market("binance", "BTCUSDT", 1, 2.0, TripleBarrier::none());
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // half filled — still working
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
        ex.on_fill(&fill(1, 1.0, 102.0, 1_100)); // now full → Open at VWAP 101
        assert_eq!(ex.state(), ExecutorState::Open);
        // read the VWAP + completing-fill timestamp through a manual flatten's outcome
        ex.cancel(&mut b);
        assert_eq!(b.markets.last().unwrap(), &("BTCUSDT".to_string(), -1, 2.0)); // closes full qty
        ex.on_fill(&fill(-1, 2.0, 101.0, 1_200));
        let o = ex.outcome().unwrap();
        assert_eq!(o.entry_px, 101.0); // (100+102)/2
        assert_eq!(o.entry_ts, 1_100); // the fill that COMPLETED the entry
        assert_eq!(o.realized_pnl, 0.0); // closed at the entry VWAP
    }

    // --- closing is idempotent to stray fills / ticks ---

    #[test]
    fn closing_ignores_further_bars_and_needs_full_close_fill() {
        let intent = PositionIntent::market(
            "binance",
            "BTCUSDT",
            1,
            2.0,
            TripleBarrier::new(None, Some(5.0), None, None), // sl @95
        );
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 2.0, 100.0, 1_000)); // Open 2 units
        ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // SL → Closing, close 2 units
        assert_eq!(ex.state(), ExecutorState::Closing);
        // a PARTIAL close fill (1 of 2) keeps it Closing
        ex.on_fill(&fill(-1, 1.0, 95.0, 1_500));
        assert_eq!(ex.state(), ExecutorState::Closing);
        // more bars while Closing submit nothing new
        ex.on_bar(&mut b, &bar(94.0, 95.0, 90.0, 91.0));
        assert_eq!(b.markets.len(), 2, "no resubmit in stage 2 — one entry + one close");
        // the remaining close fills → Closed
        ex.on_fill(&fill(-1, 1.0, 95.0, 1_600));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
    }

    // ========================================================================================
    // STAGE 4 — refresh + retry + close-reject resubmit behaviour.
    // ========================================================================================

    // --- entry retry on reject (budget + backoff, now_ms-fed) ---

    #[test]
    fn entry_reject_retries_with_backoff_then_fails_after_exhaustion() {
        // 3 total attempts, 100ms backoff. Each reject schedules a resubmit `backoff_ms` later,
        // re-driven by on_refresh against broker.now(); Failed only after the 3rd attempt is rejected.
        let intent = PositionIntent { retry: RetryPolicy::new(3, 100), ..long_tp() };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::at(1_000);
        ex.start(&mut b); // attempt 1 @ t=1000
        assert_eq!(b.markets.len(), 1);
        assert_eq!(ex.state(), ExecutorState::EntryWorking);

        // reject #1 → schedule retry at 1100 (budget 1 < 3); NOT resubmitted yet (backoff pending).
        ex.on_order_rejected(&mut b);
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
        assert_eq!(b.markets.len(), 1, "backoff not elapsed — no resubmit yet");

        // a refresh BEFORE the backoff deadline does nothing.
        b.now = 1_099;
        ex.on_refresh(&mut b);
        assert_eq!(b.markets.len(), 1);

        // AT the deadline the retry resubmits (attempt 2).
        b.now = 1_100;
        ex.on_refresh(&mut b);
        assert_eq!(b.markets.len(), 2, "retry #1 resubmitted at the backoff deadline");
        assert_eq!(ex.state(), ExecutorState::EntryWorking);

        // reject #2 → retry at 1200 (budget 2 < 3), resubmits at the deadline (attempt 3 = last).
        ex.on_order_rejected(&mut b);
        b.now = 1_200;
        ex.on_refresh(&mut b);
        assert_eq!(b.markets.len(), 3, "retry #2 resubmitted (the final attempt)");

        // reject #3 → budget 3 is NOT < 3 → Failed, no further submit.
        ex.on_order_rejected(&mut b);
        assert_eq!(ex.state(), ExecutorState::Failed);
        assert!(ex.is_terminal());
        assert_eq!(b.markets.len(), 3, "no resubmit after the budget is exhausted");
        assert_eq!(ex.outcome(), None);
    }

    #[test]
    fn zero_backoff_retry_resubmits_immediately_on_reject() {
        // backoff 0 → the retry fires inside the reject handler (already due), without an on_refresh.
        let intent = PositionIntent { retry: RetryPolicy::new(2, 0), ..long_tp() };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::at(1_000);
        ex.start(&mut b); // attempt 1
        ex.on_order_rejected(&mut b); // 0-backoff → immediate resubmit (attempt 2)
        assert_eq!(b.markets.len(), 2, "0-backoff retry resubmits immediately");
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
    }

    #[test]
    fn entry_retry_resubmit_then_fill_opens_normally() {
        // a successful resubmit after a reject opens the position and runs the lifecycle normally.
        let intent = PositionIntent { retry: RetryPolicy::new(2, 0), ..long_tp() };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::at(1_000);
        ex.start(&mut b);
        ex.on_order_rejected(&mut b); // immediate resubmit (attempt 2)
        assert_eq!(b.markets.len(), 2);
        // the resubmitted entry fills → Open (no lingering retry state).
        ex.on_fill(&fill(1, 1.0, 100.0, 1_050));
        assert_eq!(ex.state(), ExecutorState::Open);
        // and it closes on the take-profit like any normally-opened position.
        ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0));
        assert_eq!(ex.state(), ExecutorState::Closing);
        ex.on_fill(&fill(-1, 1.0, 110.0, 1_100));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::TakeProfit));
    }

    #[test]
    fn on_order_event_routes_reject_and_deny_to_retry_and_ignores_the_rest() {
        // the harness-facing dispatcher: Rejected/Denied → retry; Accepted/Canceled/Expired → no-op.
        let intent = PositionIntent { retry: RetryPolicy::new(2, 0), ..long_tp() };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b); // attempt 1
        // a RiskGate Denied is a retryable reject → immediate resubmit (attempt 2).
        ex.on_order_event(
            &mut b,
            &OrderLifecycle {
                client_order_id: "c1".into(),
                tag: None,
                kind: OrderEventKind::Denied { reason: "risk".into() },
            },
        );
        assert_eq!(b.markets.len(), 2, "Denied routed to retry");
        // Accepted / Canceled / Expired / Filled are no-ops in stage 4.
        for kind in [
            OrderEventKind::Accepted,
            OrderEventKind::Canceled { reason: String::new() },
            OrderEventKind::Expired,
            OrderEventKind::Filled,
        ] {
            ex.on_order_event(
                &mut b,
                &OrderLifecycle { client_order_id: "c".into(), tag: None, kind },
            );
        }
        assert_eq!(b.markets.len(), 2, "non-reject events do nothing");
        assert_eq!(ex.state(), ExecutorState::EntryWorking);
        // a venue Rejected now exhausts the budget (attempt 2 = max) → Failed.
        ex.on_order_event(
            &mut b,
            &OrderLifecycle {
                client_order_id: "c9".into(),
                tag: None,
                kind: OrderEventKind::Rejected { reason: "no".into() },
            },
        );
        assert_eq!(ex.state(), ExecutorState::Failed);
    }

    // --- entry refresh (reprice the resting limit) ---

    #[test]
    fn entry_refresh_reprices_resting_limit_after_interval_and_is_noop_once_open() {
        // a resting limit @99, refreshed every 5000ms (Reprice). Before the interval: nothing; at the
        // interval: reprice to broker.price(); once Open: a no-op.
        let intent = PositionIntent {
            entry: EntryKind::Limit { price: 99.0 },
            refresh: Some(RefreshPolicy::new(5_000, RefreshMode::Reprice)),
            ..long_tp()
        };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::at(1_000);
        ex.start(&mut b); // submit_limit @99 at t=1000
        assert_eq!(b.limits, vec![("BTCUSDT".to_string(), 1, 1.0, 99.0)]);

        // before every_ms elapses → no reprice.
        b.now = 5_999;
        b.px = 101.0;
        ex.on_refresh(&mut b);
        assert_eq!(b.limits.len(), 1, "interval not elapsed");

        // at the interval → reprice to the fresh mark (101).
        b.now = 6_000;
        b.px = 101.0;
        ex.on_refresh(&mut b);
        assert_eq!(b.limits.len(), 2);
        assert_eq!(
            b.limits[1],
            ("BTCUSDT".to_string(), 1, 1.0, 101.0),
            "repriced to the current broker.price()"
        );
        assert_eq!(ex.state(), ExecutorState::EntryWorking);

        // fill → Open; a later refresh is a no-op (nothing rests once Open).
        ex.on_fill(&fill(1, 1.0, 101.0, 6_050));
        assert_eq!(ex.state(), ExecutorState::Open);
        b.now = 1_000_000;
        b.px = 200.0;
        ex.on_refresh(&mut b);
        assert_eq!(b.limits.len(), 2, "no reprice once Open");
    }

    #[test]
    fn entry_refresh_cancel_replace_mode_also_reprices() {
        // CancelReplace degrades to the same portable action as Reprice (no cancel verb) — it still
        // reprices the resting limit to the fresh mark.
        let intent = PositionIntent {
            entry: EntryKind::Limit { price: 99.0 },
            refresh: Some(RefreshPolicy::new(5_000, RefreshMode::CancelReplace)),
            ..long_tp()
        };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::at(0);
        ex.start(&mut b);
        b.now = 5_000;
        b.px = 97.5;
        ex.on_refresh(&mut b);
        assert_eq!(b.limits.len(), 2);
        assert_eq!(b.limits[1], ("BTCUSDT".to_string(), 1, 1.0, 97.5));
    }

    #[test]
    fn market_entry_refresh_is_a_noop() {
        // a market intent with a refresh policy has no resting order to reprice.
        let intent = PositionIntent {
            refresh: Some(RefreshPolicy::new(1_000, RefreshMode::Reprice)),
            ..long_tp()
        };
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        b.now = 10_000;
        ex.on_refresh(&mut b);
        assert!(b.limits.is_empty());
        assert_eq!(b.markets.len(), 1, "a market entry is never repriced");
    }

    // --- close-reject sticky resubmit ---

    #[test]
    fn close_reject_resubmits_and_stays_closing() {
        let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
        ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0)); // TP → Closing, first close submitted
        assert_eq!(ex.state(), ExecutorState::Closing);
        assert_eq!(b.markets.len(), 2, "entry + first close");

        // the close is REJECTED → resubmit it and STAY Closing (sticky — never re-open).
        ex.on_order_rejected(&mut b);
        assert_eq!(ex.state(), ExecutorState::Closing);
        assert_eq!(b.markets.len(), 3, "close resubmitted");
        assert_eq!(
            b.markets[2],
            ("BTCUSDT".to_string(), -1, 1.0),
            "resubmit the full remaining flatten"
        );

        // the resubmitted close finally fills → Closed, still tagged the ORIGINAL take-profit.
        ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::TakeProfit));
    }

    #[test]
    fn close_reject_resubmits_only_the_remaining_qty() {
        // a partial close then a reject of the remainder resubmits ONLY what is left to flatten.
        let intent = PositionIntent::market(
            "binance",
            "BTCUSDT",
            1,
            2.0,
            TripleBarrier::new(None, Some(5.0), None, None), // sl @ 95
        );
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 2.0, 100.0, 1_000)); // Open 2 units
        ex.on_bar(&mut b, &bar(98.0, 99.0, 94.0, 94.5)); // SL → Closing, close 2 units
        ex.on_fill(&fill(-1, 1.0, 95.0, 1_500)); // partial close 1 of 2, still Closing
        assert_eq!(ex.state(), ExecutorState::Closing);
        ex.on_order_rejected(&mut b); // remainder rejected → resubmit only the remaining 1 unit
        assert_eq!(b.markets.last().unwrap(), &("BTCUSDT".to_string(), -1, 1.0));
        ex.on_fill(&fill(-1, 1.0, 95.0, 1_600));
        assert_eq!(ex.state(), ExecutorState::Closed);
        assert_eq!(ex.outcome().unwrap().barrier_hit, Some(BarrierKind::StopLoss));
    }

    // ========================================================================================
    // Realized-PnL law — routed through the canonical `vike_model::TradeFold` (dedup task 7).
    // ========================================================================================

    #[test]
    fn no_multiplier_realized_pnl_matches_pre_fold_behavior() {
        // default multiplier (1.0), zero fees: byte-identical to the pre-TradeFold VWAP-diff
        // formula this replaces — (exit_px − entry_px) · signed_qty · 1.0 collapses to the same
        // value the old hand-rolled math produced.
        let mut ex = PositionExecutor::new(long_tp()); // tp @ 110
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000));
        ex.on_bar(&mut b, &bar(105.0, 112.0, 104.0, 111.0)); // TP fires
        ex.on_fill(&fill(-1, 1.0, 110.0, 2_000));
        let o = ex.outcome().unwrap();
        assert_eq!(o.realized_pnl, 10.0);
        assert_eq!(o.fees, 0.0);
        assert_eq!(o.net(), 10.0);
    }

    #[test]
    fn realized_pnl_is_multiplier_aware_via_trade_fold() {
        // mult=10: entry 1@100, exit 1@110 -> gross realized = (110-100)*1*10 = 100, NOT the
        // no-multiplier 10 the old hand-rolled math would have reported.
        let intent = PositionIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
        let mut ex = PositionExecutor::new(intent).with_multiplier(10.0);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100
        ex.cancel(&mut b); // manual flatten (no barrier) -> Closing
        ex.on_fill(&fill(-1, 1.0, 110.0, 2_000)); // Closed
        let o = ex.outcome().unwrap();
        assert_eq!(o.realized_pnl, 100.0, "gross, multiplier-aware");
        assert_eq!(o.fees, 0.0);
        assert_eq!(o.net(), 100.0);
    }

    #[test]
    fn realized_pnl_is_gross_fees_reported_separately() {
        // fee-bearing round trip: `realized_pnl` stays GROSS (matches `Account`'s convention);
        // `fees` carries the round-trip cost separately, and `net()` recovers the fee-adjusted
        // figure a consumer that wants the OLD net behavior should read instead.
        let intent = PositionIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&Fill {
            side: 1,
            size: 1.0,
            price: 100.0,
            fee: 0.4,
            ts: 1_000,
            is_maker: false,
            symbol: String::new(),
        });
        ex.cancel(&mut b);
        ex.on_fill(&Fill {
            side: -1,
            size: 1.0,
            price: 110.0,
            fee: 0.6,
            ts: 2_000,
            is_maker: false,
            symbol: String::new(),
        });
        let o = ex.outcome().unwrap();
        assert_eq!(o.realized_pnl, 10.0, "gross price pnl, fees NOT netted in");
        assert_eq!(o.fees, 1.0);
        assert_eq!(o.net(), 9.0);
    }

    #[test]
    fn overshoot_close_clamps_at_flat_not_a_stray_flip() {
        // a close fill reporting MORE size than the tracked entry qty must still finish exactly
        // flat, with the pnl clamped to the tracked qty — not a stray opposite-side flip this
        // executor would then abandon untracked (the executor's contract is close-to-flat).
        let intent = PositionIntent::market("binance", "BTCUSDT", 1, 1.0, TripleBarrier::none());
        let mut ex = PositionExecutor::new(intent);
        let mut b = MockBroker::default();
        ex.start(&mut b);
        ex.on_fill(&fill(1, 1.0, 100.0, 1_000)); // Open @100, entry_qty = 1.0
        ex.cancel(&mut b); // -> Closing
        // the close fill reports 1.5 — an over-fill past the tracked 1.0 entry qty.
        ex.on_fill(&fill(-1, 1.5, 110.0, 2_000));
        assert_eq!(ex.state(), ExecutorState::Closed, "closes on the fill that covers entry_qty");
        let o = ex.outcome().unwrap();
        assert_eq!(o.realized_pnl, 10.0, "pnl clamped to the tracked 1.0, not the overshot 1.5");
    }
}
