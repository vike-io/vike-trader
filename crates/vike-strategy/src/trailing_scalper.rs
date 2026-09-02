//! `TrailingScalper` — a DIRECTIONAL two-buy / trailing-exit scalper (NOT market-making: its edge is
//! PRICE TRAVEL captured by a trailing exit, avoiding the naked-short a single-token two-sided quote implies).
//!
//! From FLAT it rests TWO orders that are BOTH cash-backed (no shorting):
//!   - a BUY on the token       (tag `"buy"`, side +1) — "buy Up"
//!   - a SELL at `mid + spread` (tag `"sell"`, side −1) — the single-token SYNTHETIC of "buy Down"
//!     (buy Down at `d` ≡ sell Up at `1−d`, identical P&L), so the resting pair is economically
//!     buy-Up + buy-Down while the backtest models it on one token.
//!
//! When EITHER side fills it:
//!   1. CANCELS both entry orders (one position at a time; never accumulates a hedged set), and
//!   2. holds with NO exit order on the book for `exit_delay_ms` — the real reaction latency: the
//!      time to LEARN you are filled and place the opposite order — then
//!   3. places a resting FLATTEN order on the side it holds. TWO exit styles:
//!      - `profit_target == 0` (default): SELL a long / BUY back a short at `mid ± spread`, re-priced
//!        each tick to track the market — takes whatever the spread gives, chases the mid.
//!      - `profit_target > 0`: a FIXED limit `entry_px ± profit_target` — only exit once the move has
//!        gone `profit_target` your way (never chases; an unreached target holds to window end).
//!   4. On the flatten fill it returns to flat and re-quotes both entries.
//!
//! The `exit_delay_ms` (default 2000) models the reaction gap IN THE STRATEGY, so the engine's own
//! order-latency model must stay OFF for this run (else the 2s is double-counted). (A run MAY
//! deliberately stack a venue-side `order_latency_ms` on top of this in-strategy gap — the
//! 2026-07-28 latency-realism spec does exactly that, modeling venue transit and trader reaction as
//! separate legs.) Trades on the
//! TICK lane (L1 quotes + L2 book) like `SpreadMaker`; the queue model fills its resting orders
//! against the real taker tape. Exit size is read from the live signed position, so a PARTIAL entry
//! fill is flattened for exactly what it opened.
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
//! already computes each market's window via `window_for`) supplies per run. `0` (absent) on EITHER
//! the delay/cutoff knob OR the open/close timestamp disables that gate entirely — never a silent
//! wrong window (a market really starting at epoch-0 is not a real scenario this codebase replays).
//! The cutoffs touch ONLY [`Phase::Quoting`] (entry placement/re-pricing/cancellation); the
//! [`Phase::Holding`] exit/flatten path reads neither knob, so a position opened before the cutoff
//! (or one that opens right at the boundary, on a late fill of an order placed before the cutoff)
//! can still be closed after it — the entire point of an exit-only tail.

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
    /// `> 0` ⇒ suppress ENTRY quoting until `market_open_ms + entry_open_delay_ms`; `0` (default)
    /// ⇒ off (entries allowed from the first tick, today's behavior). Inert unless `market_open_ms`
    /// is also set (a market's real open, not epoch-0).
    pub entry_open_delay_ms: i64,
    /// `> 0` ⇒ stop placing/re-pricing ENTRY orders once `now >= market_close_ms −
    /// entry_cutoff_before_close_ms`, cancelling any resting entries at that moment; `0` (default)
    /// ⇒ off. Inert unless `market_close_ms` is also set. Never touches the EXIT/flatten path — a
    /// held position can still close after the cutoff.
    pub entry_cutoff_before_close_ms: i64,
    /// The market's open, epoch ms. `0` (default) ⇒ `entry_open_delay_ms` is inert (unknown open).
    pub market_open_ms: i64,
    /// The market's close, epoch ms. `0` (default) ⇒ `entry_cutoff_before_close_ms` is inert
    /// (unknown close).
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

    /// Builder: arm the entry-timing cutoffs (see the module doc's "Entry-timing cutoffs" section).
    /// All-zero (the default from [`TrailingScalper::new`]) is a no-op — every entry tick is
    /// allowed, byte-identical to before this method existed.
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

    /// Registry reader convention (`BuyHold::from_params`): `qty` (default 1), `half_spread`
    /// (default 0.01), `exit_delay_ms` (default 2000), `profit_target` (default 0 = mid-following),
    /// `entry_open_delay_ms`/`entry_cutoff_before_close_ms`/`market_open_ms`/`market_close_ms` (all
    /// default 0 = the cutoffs are off). Unknown keys ignored, missing keys default.
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

    /// Whether ENTRY orders may be placed/re-priced at tick time `ts`. `true` unless a gate is
    /// BOTH armed (its own knob `> 0`) AND its reference timestamp known (`market_open_ms`/
    /// `market_close_ms` `> 0`) — so an unset gate, or a set gate with no market window supplied,
    /// never suppresses entries (the disabled-by-default contract).
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
                    // Suppressed (pre-open-delay OR past the close cutoff): pull any resting
                    // entries and go quiet. Idempotent — once `entries_live` is false this is a
                    // no-op every subsequent tick, so it never re-cancels an already-pulled pair.
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
                    if pos > 0.0 {
                        (-1, mid + half)
                    } else {
                        (1, mid - half)
                    }
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
                // A flatten fill — but only resume quoting once TRULY FLAT. A PARTIAL flatten leaves
                // a residual position; re-entering then would stack fresh buys on top of it and let
                // the position exceed `qty` (the over-accumulation leak). Stay Holding and keep
                // working the exit for the remainder (the next `step` re-prices it to `pos.abs()`).
                if HftBroker::position(broker) == 0.0 {
                    self.exit_live = false;
                    self.entries_live = false;
                    self.phase = Phase::Quoting;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vike_model::{Bar, Broker};

    /// A minimal recording `HftBroker` double: tracks every tagged submit/modify/cancel call plus
    /// a settable signed position, so the cutoff logic can be asserted call-by-call without a full
    /// fill-simulation engine. Not a fill model — `pos` is set directly by the test to simulate
    /// "already holding" for the Holding-phase assertions.
    #[derive(Default)]
    struct MockBroker {
        pos: f64,
        now_ts: i64,
        submitted: Vec<(String, i32, f64, f64)>,
        modified: Vec<(String, Option<f64>, Option<f64>)>,
        cancelled: Vec<String>,
    }

    impl Broker for MockBroker {
        fn submit_market(&mut self, _symbol: &str, _side: i32, _qty: f64) {}
        fn submit_limit(&mut self, _symbol: &str, _side: i32, _qty: f64, _price: f64) {}
        fn position(&self, _symbol: &str) -> f64 {
            self.pos
        }
        fn price(&self, _symbol: &str) -> f64 {
            0.0
        }
        fn equity(&self) -> f64 {
            0.0
        }
        fn bars(&self, _symbol: &str) -> &[Bar] {
            &[]
        }
        fn index(&self) -> usize {
            0
        }
        fn now(&self) -> i64 {
            self.now_ts
        }
    }

    impl HftBroker for MockBroker {
        fn position(&self) -> f64 {
            self.pos
        }
        fn submit_limit_tagged(&mut self, tag: &str, side: i32, qty: f64, price: f64) {
            self.submitted.push((tag.to_string(), side, qty, price));
        }
        fn modify_tagged(&mut self, tag: &str, new_qty: Option<f64>, new_price: Option<f64>) {
            self.modified.push((tag.to_string(), new_qty, new_price));
        }
        fn cancel_tagged(&mut self, tag: &str) {
            self.cancelled.push(tag.to_string());
        }
    }

    fn quote(ts: i64, bid: f64, ask: f64) -> QuoteTick {
        QuoteTick { ts, local_ts: 0, bid, ask, bid_size: 0.0, ask_size: 0.0, symbol: String::new() }
    }

    fn fill(ts: i64, side: i32, price: f64) -> Fill {
        Fill { side, size: 1.0, price, fee: 0.0, ts, is_maker: true, symbol: String::new() }
    }

    // ---- defaults-off byte-identical behavior --------------------------------------------------

    #[test]
    fn default_params_never_suppress_entries() {
        // `from_params` with an empty table must resolve every cutoff knob to 0 — inert.
        let s = TrailingScalper::from_params(&toml::Value::Table(Default::default()));
        assert_eq!(s.entry_open_delay_ms, 0);
        assert_eq!(s.entry_cutoff_before_close_ms, 0);
        assert_eq!(s.market_open_ms, 0);
        assert_eq!(s.market_close_ms, 0);
        assert!(s.entries_allowed(0));
        assert!(s.entries_allowed(i64::MAX / 2));
    }

    #[test]
    fn with_default_gates_the_first_tick_still_places_both_entries() {
        // Byte-identical reproduction of today's behavior: a fresh strategy with the cutoffs off
        // places both entries on the very first tick, exactly like before this feature existed.
        let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0);
        let mut b = MockBroker::default();
        Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
        assert_eq!(b.submitted.len(), 2, "both entries placed with cutoffs off");
        assert!(b.cancelled.is_empty());
    }

    // ---- entry_open_delay_ms --------------------------------------------------------------------

    #[test]
    fn entry_open_delay_suppresses_entries_before_the_delay_elapses() {
        let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0)
            .with_entry_gates(5_000, 0, /* market_open_ms */ 1_000, 0);
        let mut b = MockBroker::default();
        // ts = 5_999 < open(1_000) + delay(5_000) = 6_000 -> suppressed.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(5_999, 0.49, 0.51));
        assert!(b.submitted.is_empty(), "no entries before market_open + entry_open_delay_ms");
        assert!(b.cancelled.is_empty(), "nothing was resting, so nothing to cancel");
    }

    #[test]
    fn entry_open_delay_allows_entries_once_the_delay_elapses() {
        let mut s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(5_000, 0, 1_000, 0);
        let mut b = MockBroker::default();
        // ts = 6_000 == open(1_000) + delay(5_000) -> allowed (boundary is inclusive).
        Strategy::on_quote_tick(&mut s, &mut b, &quote(6_000, 0.49, 0.51));
        assert_eq!(b.submitted.len(), 2, "entries placed once the open delay has elapsed");
    }

    // ---- entry_cutoff_before_close_ms --------------------------------------------------------

    #[test]
    fn entry_cutoff_allows_entries_before_the_close_boundary() {
        let mut s =
            TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = MockBroker::default();
        // ts = 69_999 < close(100_000) - cutoff(30_000) = 70_000 -> allowed.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(69_999, 0.49, 0.51));
        assert_eq!(b.submitted.len(), 2, "entries still allowed just before the cutoff boundary");
    }

    #[test]
    fn entry_cutoff_cancels_resting_entries_at_the_boundary() {
        let mut s =
            TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = MockBroker::default();
        // First rest both entries well before the cutoff.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
        assert_eq!(b.submitted.len(), 2);
        // ts = 70_000 == close(100_000) - cutoff(30_000) -> the boundary itself is suppressed,
        // and the resting pair must be CANCELLED (never left dangling on the book).
        Strategy::on_quote_tick(&mut s, &mut b, &quote(70_000, 0.49, 0.51));
        assert_eq!(b.cancelled, vec!["buy".to_string(), "sell".to_string()]);
        // A later tick past the cutoff must NOT re-cancel (idempotent) or re-submit.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_000, 0.49, 0.51));
        assert_eq!(b.cancelled.len(), 2, "no repeated cancels on subsequent suppressed ticks");
        assert_eq!(b.submitted.len(), 2, "no re-entry after the cutoff");
    }

    #[test]
    fn entry_cutoff_never_re_prices_entries_past_the_boundary() {
        let mut s =
            TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = MockBroker::default();
        Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
        Strategy::on_quote_tick(&mut s, &mut b, &quote(75_000, 0.40, 0.60));
        assert!(b.modified.is_empty(), "past the cutoff, entries are cancelled not re-priced");
    }

    // ---- EXIT/flatten is unaffected by either cutoff -----------------------------------------

    #[test]
    fn exit_still_places_and_reprices_past_the_entry_cutoff() {
        // Arm BOTH cutoffs tight, then drive the strategy into Holding via a fill that lands
        // exactly AT the close cutoff boundary — proving the exit path ignores both gates
        // entirely (the module doc's "exit-only tail" contract).
        let mut s = TrailingScalper::new(1.0, 0.01, /* exit_delay_ms */ 2_000, 0.0)
            .with_entry_gates(5_000, 30_000, 1_000, 100_000);
        let mut b = MockBroker::default();
        // A fill at ts = 70_000 (== close - cutoff) opens a long.
        Strategy::on_fill(&mut s, &mut b, &fill(70_000, 1, 0.50));
        b.pos = 1.0; // simulate the resulting long position
        assert!(matches!(s.phase, Phase::Holding { .. }));
        // Before the reaction gap elapses (ts < entry_ts + exit_delay_ms): no exit order yet.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(71_000, 0.49, 0.51));
        assert!(b.submitted.is_empty(), "no exit order until the reaction gap elapses");
        // After the gap (ts = 72_000, well past the entry-cutoff boundary at 70_000): the flatten
        // is placed — proving the entry cutoff never blocks an exit.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(72_000, 0.49, 0.51));
        assert_eq!(b.submitted, vec![("exit".to_string(), -1, 1.0, 0.51)]);
        // A later tick re-prices the SAME exit order (modify, not a fresh submit).
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_000, 0.55, 0.60));
        assert_eq!(b.modified.len(), 1, "the flatten re-prices via modify, not a second submit");
        assert_eq!(b.submitted.len(), 1, "still exactly one exit submit");
    }

    #[test]
    fn exit_completes_and_resumes_quoting_even_though_entries_stay_cut_off() {
        // Once flat again, the strategy tries to re-quote — and correctly stays suppressed if the
        // tick is still past the close cutoff (no entries resurrected in the exit-only tail).
        let mut s = TrailingScalper::new(1.0, 0.01, 0, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = MockBroker::default();
        Strategy::on_fill(&mut s, &mut b, &fill(80_000, 1, 0.50));
        b.pos = 1.0;
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_001, 0.49, 0.51)); // places the exit
        assert_eq!(b.submitted, vec![("exit".to_string(), -1, 1.0, 0.51)]);
        // The flatten fills; position goes flat.
        b.pos = 0.0;
        Strategy::on_fill(&mut s, &mut b, &fill(80_002, -1, 0.51));
        assert!(matches!(s.phase, Phase::Quoting));
        // Back in Quoting, but ts = 80_002 is still past the close cutoff (70_000) -> no re-entry.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_003, 0.49, 0.51));
        assert_eq!(b.submitted.len(), 1, "still just the one exit submit — no fresh entries");
    }
}
