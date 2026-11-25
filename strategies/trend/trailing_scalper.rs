//! `TrailingScalper` — a DIRECTIONAL two-buy / trailing-exit scalper (NOT market-making: its edge is
//! PRICE TRAVEL captured by a trailing exit, avoiding the naked-short a single-token two-sided quote implies).
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//! why = """\
//!     NAKED SHORT on its own venue: from flat it rests a SELL leg documented as \
//!     the single-token synthetic of `buy Down` (see its module doc), an \
//!     equivalence that holds in the simulator — where a signed position may go \
//!     negative for free — and NOT on the Polymarket CLOB it was written for, \
//!     which requires the ERC-1155 outcome balance to sell. Live from flat that \
//!     leg cannot rest, so the strategy degrades to a one-sided buyer while the \
//!     backtest fills both sides; nothing in the mount path converts a sell into \
//!     the Down-buy it is priced as"""
//! routes = "single_leg"
//! optimistic_quotes = true
//!
//! [params.qty]
//! type = "number"
//! default = 1.0
//!
//! [params.half_spread]
//! type = "number"
//! default = 0.01
//!
//! [params.exit_delay_ms]
//! type = "integer"
//! default = 2000
//!
//! [params.profit_target]
//! type = "number"
//! default = 0.0
//!
//! [params.entry_open_delay_ms]
//! type = "integer"
//! default = 0
//! needs = { market_open_ms = "positive" }
//!
//! [params.entry_cutoff_before_close_ms]
//! type = "integer"
//! default = 0
//! needs = { market_close_ms = "positive" }
//!
//! [params.market_open_ms]
//! type = "integer"
//! default = 0
//! needs = { entry_open_delay_ms = "positive" }
//!
//! [params.market_close_ms]
//! type = "integer"
//! default = 0
//! needs = { entry_cutoff_before_close_ms = "positive" }
//! ```
//!
//! From FLAT it rests TWO cash-backed orders (no shorting): a BUY (tag `"buy"`, +1, "buy Up") and a
//! SELL at `mid + spread` (tag `"sell"`, −1), the single-token SYNTHETIC of "buy Down" (buy Down at
//! `d` ≡ sell Up at `1−d`, identical P&L). When EITHER fills it:
//!   1. CANCELS both entries (one position at a time; never accumulates a hedged set),
//!   2. holds with NO exit order for `exit_delay_ms` — the reaction latency (learn of the fill,
//!      place the opposite order) — then
//!   3. rests a FLATTEN on the held side: `profit_target == 0` (default) re-prices at
//!      `mid ± spread` each tick (chases the mid); `profit_target > 0` rests a FIXED
//!      `entry_price ± profit_target` (never chases; an unreached target holds to window end);
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

use vike_marketdata::{L2Book, TickQuote};
use vike_model::params::{ParamValue, Params};
use vike_model::{Fill, OrderBroker, OrderPlacement, Strategy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(TrailingScalper::from_params(params))
}

/// What each declared key RESOLVED to, in the header's order (the mount's "effective params" line).
pub fn echo(params: &Params) -> Vec<(&'static str, String)> {
    fn number(v: f64) -> String {
        format!("{v}")
    }
    let t = TrailingScalper::from_params(params);
    vec![
        ("qty", number(t.qty)),
        ("half_spread", number(t.half_spread)),
        ("exit_delay_ms", t.exit_delay_ms.to_string()),
        ("profit_target", number(t.profit_target)),
        ("entry_open_delay_ms", t.entry_open_delay_ms.to_string()),
        ("entry_cutoff_before_close_ms", t.entry_cutoff_before_close_ms.to_string()),
        ("market_open_ms", t.market_open_ms.to_string()),
        ("market_close_ms", t.market_close_ms.to_string()),
    ]
}

/// Position lifecycle.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Phase {
    /// Flat: both entry orders (`"buy"` + `"sell"`) rest.
    Quoting,
    /// Holding a position opened at `entry_timestamp`/`entry_price` on `side` (+1 long / −1 short). No exit
    /// order until `entry_timestamp + exit_delay_ms`, then a resting flatten (fixed-target or mid-following).
    Holding { entry_timestamp: i64, entry_price: f64, side: i32 },
}

/// The two-buy / delayed-flatten scalper. `qty` per side, `half_spread` off the mid, `exit_delay_ms`
/// the reaction gap, `profit_target` an optional FIXED take-profit distance off the entry price.
pub struct TrailingScalper {
    pub qty: f64,
    pub half_spread: f64,
    pub exit_delay_ms: i64,
    /// `> 0` ⇒ exit at a FIXED `entry_price ± profit_target` (only bank a move this big); `0` (default)
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
    pub fn from_params(params: &Params) -> Self {
        let f = |k: &str| params.get(k).and_then(ParamValue::as_f64);
        let i = |k: &str| params.get(k).and_then(ParamValue::as_integer);
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
    fn step<B: OrderBroker>(&mut self, broker: &mut B, mid: f64, ts: i64) {
        let half = self.half_spread;
        let limit = |price| OrderPlacement::Limit { price };
        match self.phase {
            Phase::Quoting => {
                if !self.entries_allowed(ts) {
                    // Suppressed: pull any resting entries once (idempotent via `entries_live`).
                    if self.entries_live {
                        broker.cancel("buy");
                        broker.cancel("sell");
                        self.entries_live = false;
                    }
                    return;
                }
                let (buy_price, sell_price) = (mid - half, mid + half);
                if !self.entries_live {
                    broker.place("", "buy", 1, self.qty, limit(buy_price), false);
                    broker.place("", "sell", -1, self.qty, limit(sell_price), false);
                    self.entries_live = true;
                } else {
                    broker.amend("buy", None, Some(buy_price), None);
                    broker.amend("sell", None, Some(sell_price), None);
                }
            }
            Phase::Holding { entry_timestamp: entry_ts, entry_price: entry_px, side } => {
                // NO exit order until the reaction gap elapses (the naked hold you can't act inside).
                if ts < entry_ts + self.exit_delay_ms {
                    return;
                }
                let pos = broker.position("");
                if pos == 0.0 {
                    // already flat (the flatten filled between events) — resume quoting.
                    self.phase = Phase::Quoting;
                    self.entries_live = false;
                    self.exit_live = false;
                    return;
                }
                // flatten: SELL a long / BUY back a short, sized to the ACTUAL position.
                let (exit_side, exit_price) = if self.profit_target > 0.0 {
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
                let exit_quantity = pos.abs();
                if !self.exit_live {
                    broker.place("", "exit", exit_side, exit_quantity, limit(exit_price), false);
                    self.exit_live = true;
                } else {
                    broker.amend("exit", Some(exit_quantity), Some(exit_price), None);
                }
            }
        }
    }
}

impl<B: OrderBroker> Strategy<B> for TrailingScalper {
    fn on_quote_tick(&mut self, broker: &mut B, q: &TickQuote) {
        let mid = 0.5 * (q.bid + q.ask);
        if mid > 0.0 {
            self.step(broker, mid, q.timestamp);
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
                broker.cancel("buy");
                broker.cancel("sell");
                self.entries_live = false;
                self.exit_live = false;
                self.phase = Phase::Holding {
                    entry_timestamp: fill.timestamp,
                    entry_price: fill.price,
                    side: fill.side,
                };
            }
            Phase::Holding { .. } => {
                // Resume quoting only once TRULY FLAT: re-entering on a PARTIAL flatten would
                // stack fresh buys on the residual and exceed `qty` (the over-accumulation leak).
                // The next `step` re-prices the exit to `pos.abs()`.
                if broker.position("") == 0.0 {
                    self.exit_live = false;
                    self.entries_live = false;
                    self.phase = Phase::Quoting;
                }
            }
        }
    }
}

// Copied from `crates/vike-strategy/src/strategies/trailing_scalper_tests.rs`. Run by `crates/vike-strategy-lab`
// (`cargo test -p vike-strategy-lab`); a build of this file compiles no test.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::OrderBrokerRecording;
    use std::assert_matches;

    fn quote(ts: i64, bid: f64, ask: f64) -> TickQuote {
        TickQuote {
            timestamp: ts,
            local_timestamp: 0,
            bid,
            ask,
            bid_size: 0.0,
            ask_size: 0.0,
            symbol: Default::default(),
        }
    }

    fn fill(ts: i64, side: i32, price: f64) -> Fill {
        Fill {
            side,
            size: 1.0,
            price,
            fee: 0.0,
            timestamp: ts,
            is_maker: true,
            symbol: String::new(),
            tag: None,
        }
    }

    /// Every tagged limit `b` was asked to place, as `(tag, side, qty, price)`: the rows its `tagged`
    /// held while the strategy went by `submit_limit_tagged` (decision 0277's step 6 moved it onto `place`).
    fn limits(b: &OrderBrokerRecording) -> Vec<(String, i32, f64, f64)> {
        let mut rows = Vec::new();
        for (_, tag, side, qty, order, _) in &b.placed {
            if let OrderPlacement::Limit { price } = order {
                rows.push((tag.clone(), *side, *qty, *price));
            }
        }
        rows
    }

    /// What `s` sent on ONE 0.49/0.51 quote at `ts`, its first event.
    fn after_one_quote(mut s: TrailingScalper, ts: i64) -> OrderBrokerRecording {
        let mut b = OrderBrokerRecording::default();
        Strategy::on_quote_tick(&mut s, &mut b, &quote(ts, 0.49, 0.51));
        b
    }

    // ---- defaults-off byte-identical behavior --------------------------------------------------

    #[test]
    fn default_params_never_suppress_entries() {
        // `from_params` with an empty table must resolve every cutoff knob to 0 — inert.
        let s = TrailingScalper::from_params(&Params::new());
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
        let b = after_one_quote(TrailingScalper::new(1.0, 0.01, 2000, 0.0), 0);
        assert_eq!(limits(&b).len(), 2, "both entries placed with cutoffs off");
        assert!(b.cancelled.is_empty());
    }

    // ---- entry_open_delay_ms --------------------------------------------------------------------

    #[test]
    fn entry_open_delay_suppresses_entries_before_the_delay_elapses() {
        let s = TrailingScalper::new(1.0, 0.01, 2000, 0.0)
            .with_entry_gates(5_000, 0, /* market_open_ms */ 1_000, 0);
        // ts = 5_999 < open(1_000) + delay(5_000) = 6_000 -> suppressed.
        let b = after_one_quote(s, 5_999);
        assert!(limits(&b).is_empty(), "no entries before market_open + entry_open_delay_ms");
        assert!(b.cancelled.is_empty(), "nothing was resting, so nothing to cancel");
    }

    #[test]
    fn entry_open_delay_allows_entries_once_the_delay_elapses() {
        let s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(5_000, 0, 1_000, 0);
        // ts = 6_000 == open(1_000) + delay(5_000) -> allowed (boundary is inclusive).
        let b = after_one_quote(s, 6_000);
        assert_eq!(limits(&b).len(), 2, "entries placed once the open delay has elapsed");
    }

    // ---- entry_cutoff_before_close_ms --------------------------------------------------------

    #[test]
    fn entry_cutoff_allows_entries_before_the_close_boundary() {
        let s = TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        // ts = 69_999 < close(100_000) - cutoff(30_000) = 70_000 -> allowed.
        let b = after_one_quote(s, 69_999);
        assert_eq!(limits(&b).len(), 2, "entries still allowed just before the cutoff boundary");
    }

    #[test]
    fn entry_cutoff_cancels_resting_entries_at_the_boundary() {
        let mut s =
            TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = OrderBrokerRecording::default();
        // First rest both entries well before the cutoff.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(0, 0.49, 0.51));
        assert_eq!(limits(&b).len(), 2);
        // ts = 70_000 == close(100_000) - cutoff(30_000) -> the boundary itself is suppressed,
        // and the resting pair must be CANCELLED (never left dangling on the book).
        Strategy::on_quote_tick(&mut s, &mut b, &quote(70_000, 0.49, 0.51));
        assert_eq!(b.cancelled, vec!["buy".to_string(), "sell".to_string()]);
        // A later tick past the cutoff must NOT re-cancel (idempotent) or re-submit.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_000, 0.49, 0.51));
        assert_eq!(b.cancelled.len(), 2, "no repeated cancels on subsequent suppressed ticks");
        assert_eq!(limits(&b).len(), 2, "no re-entry after the cutoff");
    }

    #[test]
    fn entry_cutoff_never_re_prices_entries_past_the_boundary() {
        let mut s =
            TrailingScalper::new(1.0, 0.01, 2000, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = OrderBrokerRecording::default();
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
        let mut b = OrderBrokerRecording::default();
        // A fill at ts = 70_000 (== close - cutoff) opens a long.
        Strategy::on_fill(&mut s, &mut b, &fill(70_000, 1, 0.50));
        b.pos = 1.0; // simulate the resulting long position
        assert_matches!(s.phase, Phase::Holding { .. });
        // Before the reaction gap elapses (ts < entry_timestamp + exit_delay_ms): no exit order yet.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(71_000, 0.49, 0.51));
        assert!(limits(&b).is_empty(), "no exit order until the reaction gap elapses");
        // After the gap (ts = 72_000, well past the entry-cutoff boundary at 70_000): the flatten
        // is placed — proving the entry cutoff never blocks an exit.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(72_000, 0.49, 0.51));
        assert_eq!(limits(&b), vec![("exit".to_string(), -1, 1.0, 0.51)]);
        // A later tick re-prices the SAME exit order (modify, not a fresh submit).
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_000, 0.55, 0.60));
        assert_eq!(b.modified.len(), 1, "the flatten re-prices via modify, not a second submit");
        assert_eq!(limits(&b).len(), 1, "still exactly one exit submit");
    }

    #[test]
    fn exit_completes_and_resumes_quoting_even_though_entries_stay_cut_off() {
        // Once flat again, the strategy tries to re-quote — and correctly stays suppressed if the
        // tick is still past the close cutoff (no entries resurrected in the exit-only tail).
        let mut s = TrailingScalper::new(1.0, 0.01, 0, 0.0).with_entry_gates(0, 30_000, 0, 100_000);
        let mut b = OrderBrokerRecording::default();
        Strategy::on_fill(&mut s, &mut b, &fill(80_000, 1, 0.50));
        b.pos = 1.0;
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_001, 0.49, 0.51)); // places the exit
        assert_eq!(limits(&b), vec![("exit".to_string(), -1, 1.0, 0.51)]);
        // The flatten fills; position goes flat.
        b.pos = 0.0;
        Strategy::on_fill(&mut s, &mut b, &fill(80_002, -1, 0.51));
        assert_matches!(s.phase, Phase::Quoting);
        // Back in Quoting, but ts = 80_002 is still past the close cutoff (70_000) -> no re-entry.
        Strategy::on_quote_tick(&mut s, &mut b, &quote(80_003, 0.49, 0.51));
        assert_eq!(limits(&b).len(), 1, "still just the one exit submit — no fresh entries");
    }
}
