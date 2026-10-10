//! `TrailingScalper` — a DIRECTIONAL two-buy / trailing-exit scalper (NOT market-making: its edge is
//! PRICE TRAVEL captured by a trailing exit, avoiding the naked-short a single-token two-sided quote implies).
//!
//! From FLAT it rests TWO cash-backed orders (no shorting): a BUY (tag `"buy"`, +1, "buy Up") and a
//! SELL at `mid + spread` (tag `"sell"`, −1), the single-token SYNTHETIC of "buy Down" (buy Down at
//! `d` ≡ sell Up at `1−d`, identical P&L). When EITHER fills it:
//!   1. CANCELS both entries (one position at a time; never accumulates a hedged set),
//!   2. holds with NO exit order for `exit_delay_ms` — the reaction latency (learn of the fill,
//!      place the opposite order) — then
//!   3. rests a FLATTEN on the held side: `profit_target == 0` (default) re-prices at
//!      `mid ± spread` each tick (chases the mid); `profit_target > 0` rests a FIXED
//!      `entry_px ± profit_target` (never chases; an unreached target holds to window end);
//!   4. on the flatten fill returns to flat and re-quotes both entries.
//!
//! `exit_delay_ms` (default 2000) models the reaction gap IN THE STRATEGY, so the engine's own
//! order-latency model must stay OFF (else it is double-counted) — unless a run deliberately stacks
//! a venue-side `order_latency_ms` on top (the 2026-07-28 latency-realism spec models venue transit
//! and trader reaction as separate legs). Trades the TICK lane (L1 + L2) like `SpreadMaker`; the
//! queue model fills against the real taker tape. Exit size is the live signed position, so a
//! PARTIAL entry fill is flattened for exactly what it opened.
//!
//! ## Entry-timing cutoffs (2026-07-28 latency-realism follow-up)
//!
//! Measured on BTC 5m up/down, last 24h, 576 tokens, honest `[end−300s, end]` window: at
//! `order_latency_ms = 0` the two-buy/trailing-exit edge is real (+61.0%/+60.0% l1/l2 summed
//! return); at the venue's verified `order_latency_ms = 250` it flips to −41.1%/−42.0% with MORE
//! churn (36.5% win vs 70.4%). One hypothesis: the strategy keeps posting fresh ENTRY pairs right up
//! to market close (a late fill under latency rides into resolution with no time to exit) and quotes
//! into the chaotic first seconds after open. Two OFF-by-default params address this — both need the
//! market's open/close, which the strategy does not otherwise see (it only observes tick timestamps):
//!
//! - `entry_open_delay_ms` (default `0` = off): suppress ENTRY quoting until
//!   `market_open_ms + entry_open_delay_ms`.
//! - `entry_cutoff_before_close_ms` (default `0` = off): stop placing/re-pricing ENTRY orders once
//!   `now >= market_close_ms − entry_cutoff_before_close_ms`, cancelling any resting entries at that
//!   moment.
//!
//! Both gates read `market_open_ms`/`market_close_ms` — explicit params the batch tool (which
//! computes each market's window via `window_for`) supplies per run. `0` (absent) on EITHER the
//! knob OR its timestamp disables that gate — never a silent wrong window. The cutoffs touch ONLY
//! `Phase::Quoting`; the `Phase::Holding` exit path reads neither, so a position opened before
//! (or right at) the cutoff can still close after it — the exit-only tail.

use vike_model::{Fill, HftBroker, L2Book, QuoteTick, Strategy};

/// Position lifecycle.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Flat: both entry orders (`"buy"` + `"sell"`) rest.
    Quoting,
    /// Holding a position opened at `entry_ts`/`entry_px` on `side` (+1 long / −1 short). No exit
    /// order until `entry_ts + exit_delay_ms`, then a resting flatten (fixed-target or mid-following).
    Holding { entry_ts: i64, entry_px: f64, side: i32 },
}

/// The two-buy / delayed-flatten scalper. `qty` per side, `half_spread` off the mid, `exit_delay_ms`
/// the reaction gap, `profit_target` an optional FIXED take-profit distance off the entry price.
pub struct TrailingScalper {
    pub qty: f64,
    pub half_spread: f64,
    pub exit_delay_ms: i64,
    /// `> 0` ⇒ exit at a FIXED `entry_px ± profit_target` (only bank a move this big); `0` (default)
    /// ⇒ the mid-following flatten.
    pub profit_target: f64,
    /// `> 0` ⇒ no ENTRY quoting before `market_open_ms + entry_open_delay_ms`; `0` (default) ⇒
    /// off. Inert unless `market_open_ms` is also set.
    pub entry_open_delay_ms: i64,
    /// `> 0` ⇒ stop placing/re-pricing ENTRY orders (cancelling resting ones) once
    /// `now >= market_close_ms − entry_cutoff_before_close_ms`; `0` (default) ⇒ off. Inert unless
    /// `market_close_ms` is also set. Never touches the EXIT path.
    pub entry_cutoff_before_close_ms: i64,
    /// The market's open, epoch ms. `0` (default) ⇒ `entry_open_delay_ms` is inert (unknown open).
    pub market_open_ms: i64,
    /// The market's close, epoch ms. `0` (default) ⇒ the close cutoff is inert (unknown close).
    pub market_close_ms: i64,
    phase: Phase,
    /// whether the two entry orders are currently resting
    entries_live: bool,
    /// whether the flatten order is currently resting
    exit_live: bool,
}

impl TrailingScalper {
    pub fn new(qty: f64, half_spread: f64, exit_delay_ms: i64, profit_target: f64) -> Self {
        TrailingScalper {
            qty,
            half_spread,
            exit_delay_ms,
            profit_target,
            entry_open_delay_ms: 0,
            entry_cutoff_before_close_ms: 0,
            market_open_ms: 0,
            market_close_ms: 0,
            phase: Phase::Quoting,
            entries_live: false,
            exit_live: false,
        }
    }

    /// Builder: arm the entry-timing cutoffs (module doc). All-zero (the [`TrailingScalper::new`]
    /// default) is a no-op.
    pub fn with_entry_gates(
        mut self,
        entry_open_delay_ms: i64,
        entry_cutoff_before_close_ms: i64,
        market_open_ms: i64,
        market_close_ms: i64,
    ) -> Self {
        self.entry_open_delay_ms = entry_open_delay_ms;
        self.entry_cutoff_before_close_ms = entry_cutoff_before_close_ms;
        self.market_open_ms = market_open_ms;
        self.market_close_ms = market_close_ms;
        self
    }

    /// Registry reader (`BuyHold::from_params` convention): each key is the field of the same name;
    /// the four cutoff keys default to `0` (off).
    pub fn from_params(params: &toml::Value) -> Self {
        let f = |k: &str| {
            params.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        };
        let i = |k: &str| params.get(k).and_then(toml::Value::as_integer);
        TrailingScalper::new(
            f("qty").unwrap_or(1.0),
            f("half_spread").unwrap_or(0.01),
            i("exit_delay_ms").unwrap_or(2000),
            f("profit_target").unwrap_or(0.0),
        )
        .with_entry_gates(
            i("entry_open_delay_ms").unwrap_or(0),
            i("entry_cutoff_before_close_ms").unwrap_or(0),
            i("market_open_ms").unwrap_or(0),
            i("market_close_ms").unwrap_or(0),
        )
    }

    /// Whether ENTRY orders may be placed/re-priced at `ts`: `true` unless a gate is BOTH armed
    /// (its knob `> 0`) AND its reference timestamp known (`> 0`) — the disabled-by-default
    /// contract.
    fn entries_allowed(&self, ts: i64) -> bool {
        let after_open = if self.entry_open_delay_ms > 0 && self.market_open_ms > 0 {
            ts >= self.market_open_ms + self.entry_open_delay_ms
        } else {
            true
        };
        let before_cutoff = if self.entry_cutoff_before_close_ms > 0 && self.market_close_ms > 0 {
            ts < self.market_close_ms - self.entry_cutoff_before_close_ms
        } else {
            true
        };
        after_open && before_cutoff
    }

    /// The shared per-tick step, driven by both the L1 quote and L2 book lanes off a resolved `mid`
    /// and event `ts`: place/re-price the two entries while flat, hold silent through the reaction
    /// gap, then place/re-price the flatten against the live position.
    fn step<B: HftBroker>(&mut self, broker: &mut B, mid: f64, ts: i64) {
        let half = self.half_spread;
        match self.phase {
            Phase::Quoting => {
                if !self.entries_allowed(ts) {
                    // Suppressed: pull any resting entries once (idempotent via `entries_live`).
                    if self.entries_live {
                        broker.cancel_tagged("buy");
                        broker.cancel_tagged("sell");
                        self.entries_live = false;
                    }
                    return;
                }
                let (buy_px, sell_px) = (mid - half, mid + half);
                if !self.entries_live {
                    broker.submit_limit_tagged("buy", 1, self.qty, buy_px);
                    broker.submit_limit_tagged("sell", -1, self.qty, sell_px);
                    self.entries_live = true;
                } else {
                    broker.modify_tagged("buy", None, Some(buy_px));
                    broker.modify_tagged("sell", None, Some(sell_px));
                }
            }
            Phase::Holding { entry_ts, entry_px, side } => {
                // NO exit order until the reaction gap elapses (the naked hold you can't act inside).
                if ts < entry_ts + self.exit_delay_ms {
                    return;
                }
                let pos = HftBroker::position(broker);
                if pos == 0.0 {
                    // already flat (the flatten filled between events) — resume quoting.
                    self.phase = Phase::Quoting;
                    self.entries_live = false;
                    self.exit_live = false;
                    return;
                }
                // flatten: SELL a long / BUY back a short, sized to the ACTUAL position.
                let (exit_side, exit_px) = if self.profit_target > 0.0 {
                    // FIXED take-profit off the entry price: only bank a move >= profit_target. An
                    // unreachable target (near a 0/1 wall) simply never fills → held to window end.
                    let px = if side > 0 {
                        entry_px + self.profit_target
                    } else {
                        entry_px - self.profit_target
                    };
                    (if side > 0 { -1 } else { 1 }, px.clamp(0.001, 0.999))
                } else {
                    // mid-following flatten (chases the market).
                    if pos > 0.0 { (-1, mid + half) } else { (1, mid - half) }
                };
                let exit_qty = pos.abs();
                if !self.exit_live {
                    broker.submit_limit_tagged("exit", exit_side, exit_qty, exit_px);
                    self.exit_live = true;
                } else {
                    broker.modify_tagged("exit", Some(exit_qty), Some(exit_px));
                }
            }
        }
    }
}

impl<B: HftBroker> Strategy<B> for TrailingScalper {
    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        let mid = 0.5 * (q.bid + q.ask);
        if mid > 0.0 {
            self.step(broker, mid, q.ts);
        }
    }

    fn on_order_book(&mut self, broker: &mut B, book: &L2Book) {
        if let Some(mid) = book.mid() {
            self.step(broker, mid, broker.now());
        }
    }

    fn on_fill(&mut self, broker: &mut B, fill: &Fill) {
        match self.phase {
            Phase::Quoting => {
                // an ENTRY filled: cancel BOTH entries (the opposite side AND the filled side's
                // remainder — one position at a time), then hold with no exit for the reaction gap.
                broker.cancel_tagged("buy");
                broker.cancel_tagged("sell");
                self.entries_live = false;
                self.exit_live = false;
                self.phase =
                    Phase::Holding { entry_ts: fill.ts, entry_px: fill.price, side: fill.side };
            }
            Phase::Holding { .. } => {
                // Resume quoting only once TRULY FLAT: re-entering on a PARTIAL flatten would
                // stack fresh buys on the residual and exceed `qty` (the over-accumulation leak).
                // The next `step` re-prices the exit to `pos.abs()`.
                if HftBroker::position(broker) == 0.0 {
                    self.exit_live = false;
                    self.entries_live = false;
                    self.phase = Phase::Quoting;
                }
            }
        }
    }
}

#[path = "trailing_scalper_tests.rs"]
#[cfg(test)]
mod trailing_scalper_tests;
