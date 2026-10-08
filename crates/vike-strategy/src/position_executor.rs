//! The position executor: the position-lifecycle TYPES, the triple-barrier MATH and the
//! [`PositionExecutor`] state machine that drives ONE position.
//!
//! Design: `docs/superpowers/specs/2026-07-11-position-executor.md`. The bottom half of a
//! Hummingbot-style *controller → PositionExecutor* split on vike's `Strategy`/`Broker` seam (the
//! top half is `controller.rs`). Here: the data a controller emits ([`PositionIntent`],
//! [`TripleBarrier`], [`EntryKind`], [`RefreshPolicy`], [`RetryPolicy`]), the state labels and
//! terminal record ([`ExecutorState`], [`ExecutorOutcome`]), the pure barrier checks
//! ([`evaluate_barriers`], [`time_barrier_hit`], …) and [`PositionExecutor`]. The executor must run
//! bit-identically on BOTH engines (backtest and live). RUST-NATIVE — no Python twin.
//!
//! ## Barrier conventions (the wire shape, frozen here)
//! - **take-profit / stop-loss / trailing** are ABSOLUTE PRICE OFFSETS (positive distance in price
//!   units) from the ENTRY FILL price; the signed barrier price is derived at check time from the
//!   entry price + side ([`take_profit_price`] / [`stop_loss_price`]). [`TripleBarrier::from_bps`]
//!   converts bps → offset against a reference price.
//! - **time_limit_ms** is a max HOLDING duration: force-exit once
//!   `now >= entry_fill_ts + time_limit_ms`. `now` is INJECTED (`broker.now()`), never a wall
//!   clock, so the predicate reproduces in backtest.
//!
//! ## Emulated, portable exits
//! Every barrier is detected over [`vike_model::order_fill_price`] — the gap-open adverse-fill /
//! trailing-ratchet law the `ConditionalBook` emulator and the backtest engines use — so a barrier
//! hit behaves bit-identically live and in backtest. No venue-native resting exit here.
//!
//! ## Realized PnL
//! [`ExecutorOutcome::realized_pnl`] is GROSS, folded through the canonical
//! [`vike_model::TradeFold`] (contract multiplier via [`PositionExecutor::with_multiplier`]); fees
//! are reported separately ([`ExecutorOutcome::fees`] / [`ExecutorOutcome::net`]).

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
    /// Rest a limit at `price` and wait for the fill (subject to [`RefreshPolicy`]).
    Limit { price: f64 },
}

/// How an unfilled LIMIT entry is refreshed while it rests ([`PositionExecutor::on_refresh`]).
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

/// How a REJECTED / DENIED entry submission is retried ([`PositionExecutor::on_order_rejected`]).
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
/// `entry`, protect it with `barriers`, refresh/retry per these policies". `#[serde(default)]` on
/// the policy fields keeps a minimal `{venue,symbol,side,qty}` JSON valid (additive).
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

/// The [`PositionExecutor`]'s lifecycle state (the transitions are on its type doc).
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
    /// Max holding duration elapsed (leg 3).
    Time,
}

/// The result of a barrier check: WHICH barrier fired and the oracle's trigger fill price.
///
/// `exit_px` is the [`vike_model::order_fill_price`] trigger price (gap-adjusted) for a price
/// barrier, or the mark (`bar.close`) for a time exit. DIAGNOSTIC only: the real close fill is the
/// venue's / sim engine's (as `ConditionalBook`'s `FiredConditional::trigger_px`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct BarrierHit {
    pub kind: BarrierKind,
    pub exit_px: f64,
}

/// The terminal record an executor produces when it reaches [`ExecutorState::Closed`];
/// `barrier_hit` is `None` for a manual flatten ([`PositionExecutor::cancel`]).
///
/// `realized_pnl` is GROSS — folded through the canonical [`vike_model::TradeFold`] (the cost-basis
/// primitive `Account`/`SimBroker`/`recon` share), contract multiplier included — matching
/// `Account`'s convention, so `fees` is reported SEPARATELY: [`net`](Self::net) is fee-adjusted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ExecutorOutcome {
    pub barrier_hit: Option<BarrierKind>,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub entry_px: f64,
    pub exit_px: f64,
    /// gross realized PnL (see the type doc).
    pub realized_pnl: f64,
    /// round-trip fees (entry + close), reported separately.
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

/// The time-barrier predicate: `true` once `now` reaches `entry_ts + time_limit_ms` (AT the deadline
/// and after, never before). Pure, injected epoch-ms; `saturating_add` guards i64 overflow.
pub fn time_barrier_hit(entry_ts: i64, time_limit_ms: i64, now: i64) -> bool {
    now >= entry_ts.saturating_add(time_limit_ms)
}

/// The exit side of a position (`+1` long / anything-else short): the OPPOSITE side, reduce-only.
#[inline]
fn exit_side(position_side: i32) -> i32 {
    if position_side >= 0 { -1 } else { 1 }
}

/// Evaluate ALL armed `barriers` for a position (`side`, `entry_px`, `entry_ts`) against `now` and
/// `bar`, returning the FIRST hit per the precedence below, or `None` if the position survives.
///
/// Price barriers go through [`vike_model::order_fill_price`] (the gap-open oracle): stop-loss as a
/// reduce-only STOP, take-profit as a reduce-only LIMIT, trailing as a TRAILING order whose extreme
/// lives in `trail_extreme`, RATCHETED in place and seeded from `entry_px` when `None` (as
/// `ConditionalBook::add_trailing` arms off the entry mark). Time uses [`time_barrier_hit`].
///
/// ## Precedence (fixed, deterministic): StopLoss → Trailing → TakeProfit → Time
/// The intrabar path is unknown, so the ADVERSE barriers are assumed to hit first (the backtest
/// engines' no-look-ahead-optimism convention): a bar straddling BOTH the stop and the target
/// resolves to the stop, and a position past its deadline that ALSO crossed a price barrier reports
/// the price barrier (time is always evaluated LAST).
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
// The PositionExecutor state machine
// ============================================================================================

/// Drives ONE position's full lifecycle over a portable [`Broker`] handle:
/// `Pending → EntryWorking → Open → Closing → Closed` (plus the terminal `Failed`/`Canceled`).
///
/// A PLAIN struct with step methods ([`start`](Self::start), [`on_bar`](Self::on_bar),
/// [`on_tick`](Self::on_tick), [`on_fill`](Self::on_fill), [`on_refresh`](Self::on_refresh),
/// [`on_order_event`](Self::on_order_event), [`cancel`](Self::cancel)) that `ControllerHarness`
/// calls from the matching `Strategy` hooks; NOT a `Strategy` itself. Every order crosses the SAME
/// `Broker` (so it still passes the live `RiskGate`), and ALL time comes from [`Broker::now`] and
/// the fill's own epoch-ms `ts` — never a wall clock — so it runs bit-identically live and in
/// backtest.
///
/// ## Transitions
/// - `Pending → EntryWorking`: [`start`](Self::start) submits the entry per [`EntryKind`].
/// - `EntryWorking → Open`: the entry fills FULLY ([`on_fill`](Self::on_fill) accumulates partials);
///   the VWAP entry price + entry timestamp are recorded and the barriers arm.
/// - `Open → Closing`: the FIRST [`BarrierHit`] from [`evaluate_barriers`] (bar) /
///   [`evaluate_barriers_at_price`] (tick) submits a market close.
/// - `Closing → Closed`: the close fills FULLY; the terminal [`ExecutorOutcome`] is produced.
/// - `EntryWorking → Failed`: the entry was rejected/denied and the [`RetryPolicy`] budget is spent.
/// - `Pending|EntryWorking → Canceled`: an external stop before any inventory ([`cancel`](Self::cancel)).
///
/// ## Sticky close (invariant)
/// Once `Closing`, the executor NEVER re-opens or re-evaluates entry/barriers:
/// [`on_bar`](Self::on_bar)/[`on_tick`](Self::on_tick) are no-ops until the close completes, so a late
/// FAVOURABLE tick can not flip a closing position back to `Open`, and a rejected CLOSE resubmits the
/// remainder and STAYS `Closing`. A position, once told to flatten, only completes the flatten (a
/// position must never be stranded).
///
/// ## Refresh / retry
/// The portable [`Broker`] has NEITHER a modify NOR a cancel verb (only `submit_*`), so BOTH
/// [`RefreshMode`] variants degrade to one fresh `submit_limit` and the stale resting entry is
/// abandoned at this layer; honoring the `mode` needs [`vike_model::HftBroker::modify_tagged`] /
/// [`vike_model::HftBroker::cancel_tagged`] (a later live-only enhancement). Retry: see
/// [`on_order_rejected`](Self::on_order_rejected).
///
/// Not here: the post-exit COOLDOWN (it sequences SUCCESSIVE executors, so it is the harness's) and
/// true reduce-only (a close is a plain opposite-side market on the portable `Broker`).
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
    /// the canonical cost-basis fold ([`vike_model::TradeFold`]) behind
    /// [`ExecutorOutcome::realized_pnl`]: entry fills, then close fills — the multiplier-aware,
    /// fee-apportioned math `Account`/`SimBroker`/`recon` share.
    fold: TradeFold,
    /// running GROSS realized PnL over the close-fill fold steps, finalized into
    /// [`ExecutorOutcome::realized_pnl`] on [`finish_closed`](Self::finish_closed).
    realized_pnl: f64,
    /// contract multiplier threaded into `fold` (default `1.0`). NOT on [`PositionIntent`] (no
    /// instrument metadata there): a caller with a non-unit contract resolves it from the
    /// instrument catalog and passes it via [`with_multiplier`](Self::with_multiplier).
    multiplier: f64,
    /// the trailing-stop extreme, seeded from the entry price and ratcheted in place each check
    trail_extreme: Option<f64>,
    /// which barrier fired (`None` for a manual flatten via [`cancel`](Self::cancel))
    barrier_hit: Option<BarrierKind>,
    /// the terminal record, set on reaching `Closed`
    outcome: Option<ExecutorOutcome>,
    /// entry submissions so far (the initial [`start`](Self::start) counts as 1); the
    /// [`RetryPolicy`] budget is exhausted once this reaches `max_attempts`.
    entry_attempts: u32,
    /// epoch-ms of the last entry (re)submit / reprice — the [`RefreshPolicy`] timer's origin.
    last_entry_submit_ms: i64,
    /// while a rejected entry backs off, the epoch-ms (`now + backoff_ms`) at/after which the retry
    /// resubmits; `None` when no retry is pending.
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

    /// The harness-driven TIMER step, per tick/bar against [`Broker::now`] (never a wall clock); a
    /// no-op in every state but `EntryWorking`:
    /// 1. **Retry** — resubmit a backed-off entry retry (scheduled by
    ///    [`on_order_rejected`](Self::on_order_rejected)) once its `backoff_ms` deadline is reached.
    /// 2. **Refresh** — RE-PRICE a still-resting, wholly-unfilled LIMIT entry to [`Broker::price`]
    ///    once [`RefreshPolicy::every_ms`] has elapsed since the last (re)submit. Not for a market
    ///    entry, while a retry is pending, or with no refresh policy armed.
    ///
    /// Both [`RefreshMode`] variants take the SAME action (a fresh `submit_limit`; the stale order is
    /// abandoned): the portable [`Broker`] has no modify/cancel verb (see the type doc).
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

    /// Route an [`OrderLifecycle`] (the harness calls this from its
    /// [`Strategy::on_order_event`](vike_model::Strategy::on_order_event)): `Rejected` / `Denied` go
    /// to [`on_order_rejected`](Self::on_order_rejected); `Accepted` / `Canceled` / `Expired` are
    /// no-ops (the refresh keys off the submit timer, not an ack).
    ///
    /// [`OrderEventKind::Filled`] is a no-op too: the fill accounting runs off
    /// [`on_fill`](Self::on_fill), which fires first and carries the quantity; acting on the
    /// completion notice would double-drive the entry/close transitions.
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

    /// React to a REJECTED / DENIED order (usually via [`on_order_event`](Self::on_order_event)). A
    /// venue `Rejected` and a `RiskGate` `Denied` are the same: the order never established, so it
    /// is retryable.
    ///
    /// - **Entry reject** (`EntryWorking`, nothing filled): while `attempts < max_attempts`, schedule
    ///   a resubmit `backoff_ms` after [`Broker::now`] (fired at once if already due, else by
    ///   [`on_refresh`](Self::on_refresh)); a spent [`RetryPolicy`] budget terminalizes `Failed`.
    /// - **Close reject** (`Closing`): resubmit the unflattened remainder and STAY `Closing` (the
    ///   sticky-close invariant: never stranded, never re-opened).
    ///
    /// A reject in any other state, or of a PARTIALLY-filled entry, is ignored (no clean single
    /// order to retry).
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
                    // the portable `Broker` has no cancel verb: a resting limit entry is abandoned
                    // here, not cancelled at the venue.
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
        // The position OPENS on the FULL entry fill.
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

        // Fold through `TradeFold` (multiplier included), clamped to the remaining unclosed qty:
        // the contract is close-to-flat, so an overfilling close (a venue/accounting anomaly)
        // clamps AT flat instead of folding a stray opposite-side flip abandoned untracked.
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
mod types_and_barrier_tests;

#[cfg(test)]
mod machine_tests;
