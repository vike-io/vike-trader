//! The position executor: the position-lifecycle TYPES and the [`PositionExecutor`] state machine
//! that drives ONE position.
//!
//! Design: `docs/superpowers/specs/2026-07-11-position-executor.md`. The bottom half of a
//! Hummingbot-style *controller → PositionExecutor* split on vike's `Strategy`/`OrderBroker` seam (the
//! top half is `controller.rs`). Here: the data a controller emits ([`PositionExecutorIntent`],
//! [`TripleBarrier`], [`PositionExecutorEntryKind`], [`PositionExecutorRefreshPolicy`], [`PositionExecutorRetryPolicy`]), the state labels and
//! terminal record ([`PositionExecutorState`], [`PositionExecutorOutcome`]) and [`PositionExecutor`]. The pure
//! barrier checks it runs ([`evaluate_barriers`], [`vike_model::time_barrier_hit`], …) are the order
//! layer's, `vike_model::strategy::triple_barrier` (decision 0269, item 3). The executor must run
//! bit-identically on BOTH engines (backtest and live). RUST-NATIVE — no Python twin.
//!
//! ## Barrier conventions (the wire shape, frozen here)
//! - **take-profit / stop-loss / trailing** are ABSOLUTE PRICE OFFSETS (positive distance in price
//!   units) from the ENTRY FILL price; the signed barrier price is derived at check time from the
//!   entry price + side ([`vike_model::take_profit_price`] / [`vike_model::stop_loss_price`]).
//!   [`TripleBarrier::from_bps`] converts bps → offset against a reference price.
//! - **time_limit_ms** is a max HOLDING duration: force-exit once
//!   `now >= entry_fill_ts + time_limit_ms`. `now` is INJECTED (`broker.now()`), never a wall
//!   clock, so the predicate reproduces in backtest.
//!
//! ## Emulated, portable exits
//! Every barrier is detected over [`vike_model::order_fill_price`] — the gap-open adverse-fill /
//! trailing-ratchet law the `OrderEmulationBook` emulator and the backtest engines use — so a barrier
//! hit behaves bit-identically live and in backtest. No venue-native resting exit here.
//!
//! ## Realized PnL
//! [`PositionExecutorOutcome::realized_pnl`] is GROSS, folded through the canonical
//! [`vike_model::TradeFold`] at the contract's value ([`PositionExecutor::with_contract_value`]: a
//! linear multiplier, or an inverse contract whose PnL is the settle coin, as the live account books
//! it); fees are reported separately ([`PositionExecutorOutcome::fees`] / [`PositionExecutorOutcome::net`]).

use serde::{Deserialize, Serialize};

use vike_marketdata::Bar;
use vike_model::{
    ContractValue, Fill, OrderBroker, OrderEvent, OrderEventKind, OrderPlacement, TradeFold,
    TripleBarrier, TripleBarrierKind, evaluate_barriers, one_price_bar,
};

/// How the executor gets INTO a position.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionExecutorEntryKind {
    /// Cross the spread now — a plain market entry.
    #[default]
    Market,
    /// Rest a limit at `price` and wait for the fill (subject to [`PositionExecutorRefreshPolicy`]).
    Limit { price: f64 },
}

/// How an unfilled LIMIT entry is refreshed while it rests ([`PositionExecutor::on_refresh`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionExecutorRefreshPolicy {
    /// Re-evaluate the resting entry every this-many ms (against `broker.now()`).
    pub every_ms: i64,
    /// What the refresh does when it fires.
    pub mode: PositionExecutorRefreshMode,
}

/// The action a [`PositionExecutorRefreshPolicy`] takes on an unfilled entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionExecutorRefreshMode {
    /// Re-price the resting entry IN PLACE (preserves venue queue priority where the venue allows).
    #[default]
    Reprice,
    /// Cancel the resting entry and submit a fresh one.
    CancelReplace,
}

impl PositionExecutorRefreshPolicy {
    pub fn new(every_ms: i64, mode: PositionExecutorRefreshMode) -> Self {
        PositionExecutorRefreshPolicy { every_ms, mode }
    }
}

/// How a REJECTED / DENIED entry submission is retried ([`PositionExecutor::on_order_rejected`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PositionExecutorRetryPolicy {
    /// Maximum TOTAL entry submissions (including the first). `1` = submit once, never retry.
    pub max_attempts: u32,
    /// Delay (ms) between successive attempts.
    pub backoff_ms: i64,
}

impl Default for PositionExecutorRetryPolicy {
    /// Single attempt, no backoff — the "don't retry" default.
    fn default() -> Self {
        PositionExecutorRetryPolicy { max_attempts: 1, backoff_ms: 0 }
    }
}

impl PositionExecutorRetryPolicy {
    pub fn new(max_attempts: u32, backoff_ms: i64) -> Self {
        PositionExecutorRetryPolicy { max_attempts, backoff_ms }
    }

    /// A single submit with no retry (same as [`Default`]).
    pub fn none() -> Self {
        PositionExecutorRetryPolicy::default()
    }
}

/// A controller's *position intent* — "open a `qty` `side` position in `(venue, symbol)`, enter via
/// `entry`, protect it with `barriers`, refresh/retry per these policies". `#[serde(default)]` on
/// the policy fields keeps a minimal `{venue,symbol,side,qty}` JSON valid (additive).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PositionExecutorIntent {
    pub venue: String,
    /// canonical symbol (resolver maps to the venue symbol at the edge)
    pub symbol: String,
    /// position direction: +1 long / -1 short
    pub side: i32,
    /// position size in units
    pub qty: f64,
    /// how to get IN
    #[serde(default)]
    pub entry: PositionExecutorEntryKind,
    /// how to get OUT (the triple barrier)
    #[serde(default)]
    pub barriers: TripleBarrier,
    /// re-price/cancel an unfilled limit entry (`None` = leave it resting)
    #[serde(default)]
    pub refresh: Option<PositionExecutorRefreshPolicy>,
    /// re-submit a rejected/denied entry
    #[serde(default)]
    pub retry: PositionExecutorRetryPolicy,
}

impl PositionExecutorIntent {
    /// A market-entry intent with barriers and default (no-retry) policies.
    pub fn market(
        venue: impl Into<String>,
        symbol: impl Into<String>,
        side: i32,
        qty: f64,
        barriers: TripleBarrier,
    ) -> Self {
        PositionExecutorIntent {
            venue: venue.into(),
            symbol: symbol.into(),
            side,
            qty,
            entry: PositionExecutorEntryKind::Market,
            barriers,
            refresh: None,
            retry: PositionExecutorRetryPolicy::default(),
        }
    }
}

/// The [`PositionExecutor`]'s lifecycle state (the transitions are on its type doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionExecutorState {
    /// Created from an intent; entry not yet submitted.
    Pending,
    /// Entry order in flight / resting (drives refresh + submit-retry).
    EntryWorking,
    /// Position live; barriers armed and evaluated each tick/bar.
    Open,
    /// A barrier fired; the close order is in flight. STICKY — a partial flatten re-submits the
    /// remainder until flat (a position must never be stranded).
    Closing,
    /// Flat. Terminal (records a [`PositionExecutorOutcome`]).
    Closed,
    /// Entry never landed (retries exhausted). Terminal, no position.
    Failed,
    /// Controller stopped it before any fill. Terminal, no position.
    Canceled,
}

/// The terminal record an executor produces when it reaches [`PositionExecutorState::Closed`];
/// `barrier_hit` is `None` for a manual flatten ([`PositionExecutor::cancel`]).
///
/// `realized_pnl` is GROSS — folded through the canonical [`vike_model::TradeFold`] (the cost-basis
/// primitive `Account`/`OrderBrokerSimulator`/`recon` share) at the contract's value — matching `Account`'s
/// convention, so `fees` is reported SEPARATELY: [`net`](Self::net) is fee-adjusted. On an INVERSE
/// contract `realized_pnl` is in the settle coin.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PositionExecutorOutcome {
    pub barrier_hit: Option<TripleBarrierKind>,
    pub entry_timestamp: i64,
    pub exit_timestamp: i64,
    pub entry_price: f64,
    pub exit_price: f64,
    /// gross realized PnL (see the type doc).
    pub realized_pnl: f64,
    /// round-trip fees (entry + close), reported separately.
    pub fees: f64,
}

impl PositionExecutorOutcome {
    /// Realized PnL net of round-trip fees (`realized_pnl - fees`).
    pub fn net(&self) -> f64 {
        self.realized_pnl - self.fees
    }
}

/// The exit side of a position (`+1` long / anything-else short): the OPPOSITE side, reduce-only.
#[inline]
fn exit_side(position_side: i32) -> i32 {
    if position_side >= 0 { -1 } else { 1 }
}

// ============================================================================================
// The PositionExecutor state machine
// ============================================================================================

/// Drives ONE position's full lifecycle over the order seam ([`OrderBroker`]):
/// `Pending → EntryWorking → Open → Closing → Closed` (plus the terminal `Failed`/`Canceled`).
///
/// A PLAIN struct with step methods ([`start`](Self::start), [`on_bar`](Self::on_bar),
/// [`on_tick`](Self::on_tick), [`on_fill`](Self::on_fill), [`on_refresh`](Self::on_refresh),
/// [`on_order_event`](Self::on_order_event), [`cancel`](Self::cancel)) that `ControllerHarness`
/// calls from the matching `Strategy` hooks; NOT a `Strategy` itself. Every order crosses the SAME
/// broker (so it still passes the live `RiskGate`), as an UNTAGGED [`OrderBroker::place`] on the
/// intent's symbol (the untagged lanes and events of the old `submit_market`/`submit_limit`), and
/// ALL time comes from `broker.now()` and the fill's own epoch-ms `ts` — never a wall clock — so it
/// runs bit-identically live and in backtest.
///
/// ## Transitions
/// - `Pending → EntryWorking`: [`start`](Self::start) submits the entry per [`PositionExecutorEntryKind`].
/// - `EntryWorking → Open`: the entry fills FULLY ([`on_fill`](Self::on_fill) accumulates partials);
///   the VWAP entry price + entry timestamp are recorded and the barriers arm.
/// - `Open → Closing`: the FIRST [`vike_model::TripleBarrierHit`] from [`evaluate_barriers`] (bar) /
///   [`vike_model::evaluate_barriers_at_price`] (tick) submits a market close.
/// - `Closing → Closed`: the close fills FULLY; the terminal [`PositionExecutorOutcome`] is produced.
/// - `EntryWorking → Failed`: the entry was rejected/denied and the [`PositionExecutorRetryPolicy`] budget is spent.
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
/// A LIMIT entry with a [`PositionExecutorRefreshPolicy`] rests under the executor's tag (`<symbol>:entry`; the
/// harness keeps one executor per symbol), so the refresh reaches the order itself:
/// [`PositionExecutorRefreshMode::Reprice`] amends its price in place, [`PositionExecutorRefreshMode::CancelReplace`] cancels it and
/// places what is left of the entry afresh on the NEXT hook (never in the cancel's own hook: the live
/// drain sends a hook's submits before its cancels, so that cancel would pull the new order), and
/// [`cancel`](Self::cancel) pulls it. Every other entry is untagged and is never amended or
/// cancelled. Retry: see [`on_order_rejected`](Self::on_order_rejected).
///
/// Not here: the post-exit COOLDOWN (it sequences SUCCESSIVE executors, so it is the harness's) and
/// true reduce-only (a close is a plain opposite-side market, decision 0269 item 3).
#[derive(Debug)]
pub struct PositionExecutor {
    intent: PositionExecutorIntent,
    state: PositionExecutorState,
    /// accumulated entry-fill qty (handles partial entry fills)
    entry_quantity: f64,
    /// the running average of the entry fills, by the contract's averaging law
    /// ([`ContractValue::average_price`]: harmonic on an inverse contract)
    entry_average: f64,
    /// entry VWAP, finalised on the `EntryWorking → Open` transition
    entry_price: f64,
    /// epoch-ms of the fill that COMPLETED the entry — the time barrier's origin
    entry_timestamp: i64,
    /// accumulated close-fill qty
    exit_quantity: f64,
    /// the running average of the close fills, by the same law
    exit_average: f64,
    /// exit VWAP, finalised on the `Closing → Closed` transition
    exit_price: f64,
    /// epoch-ms of the fill that completed the close
    exit_timestamp: i64,
    /// round-trip fees accumulated across entry + close fills
    fees: f64,
    /// the canonical cost-basis fold ([`vike_model::TradeFold`]) behind
    /// [`PositionExecutorOutcome::realized_pnl`]: entry fills, then close fills — the value-aware,
    /// fee-apportioned math `Account`/`OrderBrokerSimulator`/`recon` share.
    fold: TradeFold,
    /// running GROSS realized PnL over the close-fill fold steps, finalized into
    /// [`PositionExecutorOutcome::realized_pnl`] on [`finish_closed`](Self::finish_closed).
    realized_pnl: f64,
    /// the contract value threaded into `fold` (default linear `1.0`). NOT on [`PositionExecutorIntent`] (no
    /// instrument metadata there): `ControllerHarness` passes the broker's
    /// `contract_value` via [`with_contract_value`](Self::with_contract_value).
    value: ContractValue,
    /// the trailing-stop extreme, seeded from the entry price and ratcheted in place each check
    trail_extreme: Option<f64>,
    /// which barrier fired (`None` for a manual flatten via [`cancel`](Self::cancel))
    barrier_hit: Option<TripleBarrierKind>,
    /// the terminal record, set on reaching `Closed`
    outcome: Option<PositionExecutorOutcome>,
    /// entry submissions so far (the initial [`start`](Self::start) counts as 1); the
    /// [`PositionExecutorRetryPolicy`] budget is exhausted once this reaches `max_attempts`.
    entry_attempts: u32,
    /// epoch-ms of the last entry (re)submit / reprice — the [`PositionExecutorRefreshPolicy`] timer's origin.
    last_entry_submit_ms: i64,
    /// while a rejected entry backs off, the epoch-ms (`now + backoff_ms`) at/after which the retry
    /// resubmits; `None` when no retry is pending.
    retry_at_ms: Option<i64>,
    /// A [`PositionExecutorRefreshMode::CancelReplace`] pulled the tagged entry; the next hook places what is left of
    /// it afresh.
    replace_pending: bool,
}

impl PositionExecutor {
    /// Create a `Pending` executor for `intent` — nothing is submitted until [`start`](Self::start).
    pub fn new(intent: PositionExecutorIntent) -> Self {
        PositionExecutor {
            intent,
            state: PositionExecutorState::Pending,
            entry_quantity: 0.0,
            entry_average: 0.0,
            entry_price: 0.0,
            entry_timestamp: 0,
            exit_quantity: 0.0,
            exit_average: 0.0,
            exit_price: 0.0,
            exit_timestamp: 0,
            fees: 0.0,
            fold: TradeFold::default(),
            realized_pnl: 0.0,
            value: ContractValue::default(),
            trail_extreme: None,
            barrier_hit: None,
            outcome: None,
            entry_attempts: 0,
            last_entry_submit_ms: 0,
            retry_at_ms: None,
            replace_pending: false,
        }
    }

    /// Override the contract value the realized-pnl [`TradeFold`] uses (default linear `1.0`). Call
    /// before [`start`](Self::start) — e.g.
    /// `PositionExecutor::new(intent).with_contract_value(ContractValue::linear(10.0))`. A linear
    /// value folds bit for bit as the bare multiplier did ([`TradeFold::apply_value`]).
    pub fn with_contract_value(mut self, value: ContractValue) -> Self {
        self.value = value;
        self
    }

    /// The current lifecycle state.
    pub fn state(&self) -> PositionExecutorState {
        self.state
    }

    /// The intent this executor is driving.
    pub fn intent(&self) -> &PositionExecutorIntent {
        &self.intent
    }

    /// The terminal outcome — `Some` once the position round-trip reached `Closed`; `None` while live
    /// and for the `Failed`/`Canceled` terminals (no position was ever held).
    pub fn outcome(&self) -> Option<PositionExecutorOutcome> {
        self.outcome
    }

    /// `true` once the executor reached a terminal state (`Closed`/`Failed`/`Canceled`).
    pub fn is_terminal(&self) -> bool {
        matches!(
            self.state,
            PositionExecutorState::Closed
                | PositionExecutorState::Failed
                | PositionExecutorState::Canceled
        )
    }

    /// Submit the entry per [`PositionExecutorIntent::entry`] and move `Pending → EntryWorking`. Idempotent —
    /// a call in any non-`Pending` state is a no-op.
    pub fn start<B: OrderBroker>(&mut self, broker: &mut B) {
        if self.state != PositionExecutorState::Pending {
            return;
        }
        let now = broker.now();
        self.submit_entry(broker, now);
        self.state = PositionExecutorState::EntryWorking;
    }

    /// Evaluate the barriers against a closed `bar` while `Open` (a no-op in every other state — the
    /// sticky-close guard). On the first [`vike_model::TripleBarrierHit`] submit the market close and move
    /// to `Closing`.
    pub fn on_bar<B: OrderBroker>(&mut self, broker: &mut B, bar: &Bar) {
        self.close_on_barrier(broker, |_| bar);
    }

    /// Evaluate the barriers against a single tick price `px` while `Open` (a no-op otherwise) — the
    /// tick twin of [`on_bar`](Self::on_bar), a degenerate one-price bar mirroring
    /// `OrderEmulationBook::check_price` (exactly [`vike_model::evaluate_barriers_at_price`]'s bar).
    pub fn on_tick<B: OrderBroker>(&mut self, broker: &mut B, px: f64) {
        self.close_on_barrier(broker, |now| one_price_bar(now, px));
    }

    /// The step [`on_bar`](Self::on_bar) and [`on_tick`](Self::on_tick) share: a no-op unless
    /// `Open`; otherwise judge the barriers against the bar `bar_at` builds at `broker.now()` and,
    /// on the first [`vike_model::TripleBarrierHit`], submit the market close and move to `Closing`.
    fn close_on_barrier<B: OrderBroker, T: std::borrow::Borrow<Bar>>(
        &mut self,
        broker: &mut B,
        bar_at: impl FnOnce(i64) -> T,
    ) {
        if self.state != PositionExecutorState::Open {
            return;
        }
        let now = broker.now();
        if let Some(hit) = evaluate_barriers(
            self.entry_price,
            self.intent.side,
            self.entry_timestamp,
            &self.intent.barriers,
            now,
            bar_at(now).borrow(),
            &mut self.trail_extreme,
        ) {
            self.begin_close(broker, Some(hit.kind));
        }
    }

    /// The harness-driven TIMER step, per tick/bar against `broker.now()` (never a wall clock); a
    /// no-op in every state but `EntryWorking`:
    /// 1. **Retry** — resubmit a backed-off entry retry (scheduled by
    ///    [`on_order_rejected`](Self::on_order_rejected)) once its `backoff_ms` deadline is reached.
    /// 2. **Replace** — place afresh, at `broker.price(symbol)`, what is left of an entry a
    ///    [`PositionExecutorRefreshMode::CancelReplace`] pulled on an earlier hook.
    /// 3. **Refresh** — once [`PositionExecutorRefreshPolicy::every_ms`] has elapsed since the last (re)submit, a
    ///    still-resting, wholly-unfilled LIMIT entry is amended to `broker.price(symbol)`
    ///    ([`PositionExecutorRefreshMode::Reprice`]) or cancelled to be replaced on the next hook
    ///    ([`PositionExecutorRefreshMode::CancelReplace`]). Not for a market entry, while a retry is pending, or with
    ///    no refresh policy armed (see the type doc).
    pub fn on_refresh<B: OrderBroker>(&mut self, broker: &mut B) {
        let now = broker.now();
        // (1) a backed-off entry retry that has become due.
        self.drive_entry_retry(broker, now);
        if self.state != PositionExecutorState::EntryWorking {
            return;
        }
        // (2) the second half of a cancel-replace, one hook after its cancel.
        if self.replace_pending && self.retry_at_ms.is_none() {
            self.replace_pending = false;
            let order = OrderPlacement::Limit { price: broker.price(&self.intent.symbol) };
            let rest = self.intent.qty - self.entry_quantity;
            broker.place(
                &self.intent.symbol,
                &self.entry_tag(),
                self.intent.side,
                rest,
                order,
                false,
            );
            self.last_entry_submit_ms = now;
            return;
        }
        // (3) refresh a resting, unfilled LIMIT entry whose refresh interval elapsed.
        if self.retry_at_ms.is_some() || self.entry_quantity > 0.0 {
            // awaiting a retry (nothing is resting) or partially filled (leave the remainder alone).
            return;
        }
        if !matches!(self.intent.entry, PositionExecutorEntryKind::Limit { .. }) {
            return; // a market entry has no resting order to reprice.
        }
        let Some(policy) = self.intent.refresh else {
            return; // no refresh policy armed.
        };
        if now < self.last_entry_submit_ms.saturating_add(policy.every_ms) {
            return; // the refresh interval has not elapsed.
        }
        let new_price = broker.price(&self.intent.symbol);
        let tag = self.entry_tag();
        match policy.mode {
            PositionExecutorRefreshMode::Reprice => broker.amend(&tag, None, Some(new_price), None),
            PositionExecutorRefreshMode::CancelReplace => {
                broker.cancel(&tag);
                self.replace_pending = true;
            }
        }
        self.last_entry_submit_ms = now;
    }

    /// Fold one execution into the lifecycle. The STATE disambiguates it: an `EntryWorking` fill is an
    /// entry fill (accumulated; a FULL fill opens the position), a `Closing` fill is a close fill
    /// (accumulated; a full flatten produces the terminal [`PositionExecutorOutcome`]). Fills in any other
    /// state are ignored (defensive — nothing is in flight).
    pub fn on_fill(&mut self, fill: &Fill) {
        match self.state {
            PositionExecutorState::EntryWorking => self.apply_entry_fill(fill),
            PositionExecutorState::Closing => self.apply_close_fill(fill),
            _ => {}
        }
    }

    /// Route an [`OrderEvent`] (the harness calls this from its
    /// [`Strategy::on_order_event`](vike_model::Strategy::on_order_event)): `Rejected` / `Denied` go
    /// to [`on_order_rejected`](Self::on_order_rejected); `Accepted` / `Canceled` / `Expired` are
    /// no-ops (the refresh keys off the submit timer, not an ack).
    ///
    /// [`OrderEventKind::Filled`] is a no-op too: the fill accounting runs off
    /// [`on_fill`](Self::on_fill), which fires first and carries the quantity; acting on the
    /// completion notice would double-drive the entry/close transitions.
    pub fn on_order_event<B: OrderBroker>(&mut self, broker: &mut B, event: &OrderEvent) {
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
    ///   a resubmit `backoff_ms` after `broker.now()` (fired at once if already due, else by
    ///   [`on_refresh`](Self::on_refresh)); a spent [`PositionExecutorRetryPolicy`] budget terminalizes `Failed`.
    /// - **Close reject** (`Closing`): resubmit the unflattened remainder and STAY `Closing` (the
    ///   sticky-close invariant: never stranded, never re-opened).
    ///
    /// A reject in any other state, or of a PARTIALLY-filled entry, is ignored (no clean single
    /// order to retry).
    pub fn on_order_rejected<B: OrderBroker>(&mut self, broker: &mut B) {
        let now = broker.now();
        match self.state {
            PositionExecutorState::EntryWorking => {
                if self.entry_quantity > 0.0 {
                    // a partially-filled entry's remainder was rejected — don't spawn a duplicate
                    // full-size retry; leave it working for the harness/controller to resolve.
                    return;
                }
                if self.entry_attempts < self.intent.retry.max_attempts {
                    self.retry_at_ms = Some(now.saturating_add(self.intent.retry.backoff_ms));
                    self.drive_entry_retry(broker, now); // fire now if the backoff is already due
                } else {
                    self.state = PositionExecutorState::Failed;
                }
            }
            PositionExecutorState::Closing => {
                // sticky close: re-submit the remaining flatten, never re-open.
                let remaining = self.entry_quantity - self.exit_quantity;
                if remaining > 0.0 {
                    let exit = exit_side(self.intent.side);
                    let order = OrderPlacement::Market;
                    broker.place(&self.intent.symbol, "", exit, remaining, order, false);
                }
            }
            _ => {}
        }
    }

    /// External stop (the controller pulled this executor). Before any inventory exists it
    /// terminalizes as `Canceled`; if the position is already (partly) open it FLATTENS instead
    /// (submits the market close → `Closing`) so a stop never strands inventory. A no-op once already
    /// `Closing`/terminal.
    pub fn cancel<B: OrderBroker>(&mut self, broker: &mut B) {
        match self.state {
            PositionExecutorState::Pending => self.state = PositionExecutorState::Canceled,
            PositionExecutorState::EntryWorking => {
                self.pull_entry(broker);
                if self.entry_quantity > 0.0 {
                    // a partial entry already left inventory — flatten it, don't strand it
                    self.begin_close(broker, None);
                } else {
                    // an UNTAGGED resting limit entry cannot be cancelled: it is abandoned here,
                    // not cancelled at the venue.
                    self.state = PositionExecutorState::Canceled;
                }
            }
            PositionExecutorState::Open => self.begin_close(broker, None),
            _ => {}
        }
    }

    // ---- internals ----

    /// Submit the entry per [`PositionExecutorIntent::entry`], counting the attempt and stamping the refresh
    /// timer's origin (`now`). Shared by [`start`](Self::start) and the retry resubmit.
    fn submit_entry<B: OrderBroker>(&mut self, broker: &mut B, now: i64) {
        let side = self.intent.side;
        let qty = self.intent.qty;
        let order = match self.intent.entry {
            PositionExecutorEntryKind::Market => OrderPlacement::Market,
            PositionExecutorEntryKind::Limit { price } => OrderPlacement::Limit { price },
        };
        broker.place(&self.intent.symbol, &self.entry_tag(), side, qty, order, false);
        self.entry_attempts += 1;
        self.last_entry_submit_ms = now;
        self.replace_pending = false; // a fresh entry went out
    }

    /// The tag a REFRESHABLE entry (a limit with a [`PositionExecutorRefreshPolicy`]) rests under, so the refresh
    /// and [`cancel`](Self::cancel) can name it; `""` (untagged) for every other entry.
    fn entry_tag(&self) -> String {
        match (self.intent.entry, self.intent.refresh) {
            (PositionExecutorEntryKind::Limit { .. }, Some(_)) => {
                format!("{}:entry", self.intent.symbol)
            }
            _ => String::new(),
        }
    }

    /// Cancel the resting entry by its tag. Only a refreshable entry has one, and nothing rests while
    /// its replacement waits for the next hook or a rejected entry backs off.
    fn pull_entry<B: OrderBroker>(&mut self, broker: &mut B) {
        let tag = self.entry_tag();
        if !tag.is_empty() && !self.replace_pending && self.retry_at_ms.is_none() {
            broker.cancel(&tag);
        }
    }

    /// Resubmit a pending entry retry once its backoff deadline `retry_at_ms` is reached — only while
    /// `EntryWorking` (a fill/terminal in the meantime cancels it). Clears the pending mark and
    /// re-stamps the refresh timer via [`submit_entry`](Self::submit_entry).
    fn drive_entry_retry<B: OrderBroker>(&mut self, broker: &mut B, now: i64) {
        if self.state != PositionExecutorState::EntryWorking {
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
    fn begin_close<B: OrderBroker>(&mut self, broker: &mut B, kind: Option<TripleBarrierKind>) {
        let exit = exit_side(self.intent.side);
        broker.place(
            &self.intent.symbol,
            "",
            exit,
            self.entry_quantity,
            OrderPlacement::Market,
            false,
        );
        self.barrier_hit = kind;
        self.state = PositionExecutorState::Closing;
    }

    fn apply_entry_fill(&mut self, fill: &Fill) {
        if self.entry_quantity + fill.size > 0.0 {
            self.entry_average = self.value.average_price(
                self.entry_quantity,
                self.entry_average,
                fill.size,
                fill.price,
            );
        }
        self.entry_quantity += fill.size;
        self.fees += fill.fee;
        self.fold.apply_value(
            self.intent.side,
            fill.size,
            fill.price,
            fill.fee,
            fill.timestamp,
            self.value,
        );
        // The position OPENS on the FULL entry fill.
        if self.entry_quantity >= self.intent.qty {
            self.entry_price = self.entry_average;
            self.entry_timestamp = fill.timestamp;
            self.retry_at_ms = None; // opened — drop any pending entry retry
            self.state = PositionExecutorState::Open;
        }
    }

    fn apply_close_fill(&mut self, fill: &Fill) {
        if self.exit_quantity + fill.size > 0.0 {
            self.exit_average = self.value.average_price(
                self.exit_quantity,
                self.exit_average,
                fill.size,
                fill.price,
            );
        }
        self.exit_quantity += fill.size;
        self.fees += fill.fee;

        // Fold through `TradeFold` (at the contract value), clamped to the remaining unclosed qty:
        // the contract is close-to-flat, so an overfilling close (a venue/accounting anomaly)
        // clamps AT flat instead of folding a stray opposite-side flip abandoned untracked.
        let already_closed = self.exit_quantity - fill.size;
        let remaining = (self.entry_quantity - already_closed).max(0.0);
        let closing_qty = fill.size.min(remaining);
        if closing_qty > 0.0 {
            let step = self.fold.apply_value(
                exit_side(self.intent.side),
                closing_qty,
                fill.price,
                fill.fee,
                fill.timestamp,
                self.value,
            );
            if let Some(closed) = step.closed {
                self.realized_pnl += closed.pnl;
            }
        }

        // Flat once the close has covered the tracked entry qty.
        if self.exit_quantity >= self.entry_quantity {
            self.exit_price = self.exit_average;
            self.exit_timestamp = fill.timestamp;
            self.finish_closed();
        }
    }

    fn finish_closed(&mut self) {
        self.outcome = Some(PositionExecutorOutcome {
            barrier_hit: self.barrier_hit,
            entry_timestamp: self.entry_timestamp,
            exit_timestamp: self.exit_timestamp,
            entry_price: self.entry_price,
            exit_price: self.exit_price,
            realized_pnl: self.realized_pnl,
            fees: self.fees,
        });
        self.state = PositionExecutorState::Closed;
    }
}

#[cfg(test)]
mod types_and_barrier_tests;

#[cfg(test)]
pub(crate) mod machine_tests;
