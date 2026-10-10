//! `cheap_np` — the Polymarket BTC 5-minute up/down fair-value mispricing TAKER.
//!
//! Port of the live bot `vike_db_data_jobs/trading/fair_value_bot/cheap_bot.py` (the oracle;
//! read-only; entry rule `FairValueCheapBot._cheap_tape_entry`), math in
//! [`crate::strategies::fair_value`]. `_np` = **N**o-**P**ersistence: the cheap leg fires on the
//! now-edge alone, with no 5-second confirmation.
//!
//! # The rule
//!
//! Each 5-minute window is two ERC-1155 outcome tokens (`outcome_index` 0 = Up, 1 = Down) summing
//! to $1. Enter on the FIRST taker-BUY print of either token that clears the whole gate — cheap
//! band `[0.10, 0.35)`, time-till-expiry in `[15, 270]` s, dual-β worst-case edge strictly above
//! θ = 0.055 evaluated at THAT PRINT's own spot and time (not the scan moment) — then hold to
//! resolution.
//!
//! TAPE-driven, not book-driven (`cheap_bot.py:_find_entry`): the favourable ask is usually taken
//! before it ever rests, so polling the book enters windows the backtest never saw.
//!
//! ⚠ A tape strategy: it submits only from `on_trade_tick` (`on_bar`/`on_quote_tick` only feed
//! spot), which the param-gate harness never drives — hence `SIMULATOR_ONLY` and no
//! `ParamKeys::Declared` row (`docs/decisions/0075`).
//!
//! # Modes
//!
//! * [`CheapNpMode::Hold`] — the live behaviour and what the reference numbers encode: one entry
//!   per window, held to resolution. Settlement is the ENGINE's
//!   (`vike_sim::EngineParams::resolution` + `SimBroker::settle_at_payout`); never an exit here.
//! * [`CheapNpMode::Flip`] — momentum-follow (`trading/fair_value/flip_cheap.py`): every later
//!   **run start** (a qualifying print whose `outcome_index` differs from the window's previous
//!   qualifying row) exits the held token and enters the other. Multi-flip, like `cheap_bot.py`.
//!
//! # Symbol convention
//!
//! The model layer has no instrument expiry, so the window's `sts` — hence `t` and the token
//! pairing — comes from the slug, as in the Python; one instance spans thousands of windows:
//!
//! ```text
//! btc-updown-5m-1772323200#0     <slug>#<outcome_index>
//!                ^^^^^^^^^^ ^    sts (must be a multiple of 300 — the Python's own guard)
//! ```
//!
//! A symbol that does not parse is never traded, so the spot series can be fed as a symbol safely.
//!
//! # Feed ordering
//!
//! Every decision derives from the tick payload and the spot samples seen SO FAR, so the strategy
//! is correct under both replay orderings (`TickReplayConfig::feed_latency`: false = venue clock,
//! true = arrival clock, the latency haircut being measured); nothing reads a future sample.
//! "First qualifying print per window" resolves in ARRIVAL order — the honest live semantic.

use std::collections::VecDeque;

use vike_model::fair::UPDOWN_WINDOW_SECS;
use vike_model::{Bar, Broker, QuoteTick, Strategy, TradeTick};

use crate::strategies::cheap_np_ask::{edge_at, price_at_edge, prob_wc};
use crate::strategies::fair_value::{
    H, SIGMA_LOOKBACK_S, THETA, cheap_gate, cheap_time_ok, in_cheap_band, price_at, trailing_sigma,
};

/// The probe size [`CheapNp::resolve_ask`] uses to tell "no book" from "nothing worth lifting".
/// A sentinel, never ordered: any positive value works (a book walk takes `min(level, need)`).
const BOOK_PROBE_QTY: f64 = 1e-9;

/// WHICH PRICE the entry is gated and priced on. The print is what somebody else already took, so
/// gating on it scores the edge 4–8 cents too generously against a 5.5 cent bar; the measurement
/// and the layering are [`crate::strategies::cheap_np_ask`]'s.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryPrice {
    /// The fired print's own price (the live behaviour; every published reference number encodes
    /// it). THE DEFAULT, so the 12,639-entry parity gate is untouched.
    #[default]
    Print,
    /// The RESTING ASK from the broker's book: the print is a SIGNAL ONLY, the edge is re-scored at
    /// the obtainable price and the entry taken only if it STILL clears θ, sized to in-edge depth.
    ///
    /// ⚠ Needs a broker answering [`Broker::quote_vwap`] / [`Broker::depth_within_price`] (a
    /// `run_ticks` replay with `Tick::Book` events). Otherwise every signal is refused and counted
    /// in [`CheapNp::ask_no_book`], never priced at the print: a run that forgot to feed the book
    /// must look like zero entries, never like a good backtest.
    RestingAsk,
}

/// What the strategy does after its first entry in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CheapNpMode {
    /// Enter once, hold to resolution (the live `cheap_np` leg).
    #[default]
    Hold,
    /// Enter, then flip to the opposite token on every later qualifying opposite-side print
    /// (`trading/fair_value/flip_cheap.py` run-start semantics, multi-flip).
    Flip,
}

/// One market's identity as parsed out of a token symbol `"<slug>#<outcome_index>"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenId {
    /// Window open, epoch SECONDS — the trailing `-<sts>` of the slug.
    pub sts: i64,
    /// 0 = Up, 1 = Down.
    pub oidx: u8,
}

impl TokenId {
    /// Parse `"btc-updown-5m-1772323200#0"`. Checks ONLY the `#<0|1>` suffix, a trailing
    /// `-<integer>` slug segment, and `sts` a whole multiple of [`UPDOWN_WINDOW_SECS`] (the
    /// Python's `sts % 300 == 0` grid guard). ⚠ ANY prefix passes, and `sport_taker`'s
    /// `<condition_id>#<outcome_index>` shares the `#` suffix.
    pub fn parse(symbol: &str) -> Option<TokenId> {
        let (slug, idx) = symbol.rsplit_once('#')?;
        let oidx: u8 = idx.parse().ok()?;
        if oidx > 1 {
            return None;
        }
        let (_, sts_str) = slug.rsplit_once('-')?;
        let sts: i64 = sts_str.parse().ok()?;
        (sts > 0 && sts % UPDOWN_WINDOW_SECS == 0).then_some(TokenId { sts, oidx })
    }
}

/// One print the gate fired on, recorded as it fires — the SIGNAL, not the execution.
///
/// The engine fills a market order at the token's NEXT print (or never), so the price paid is not
/// `ask`. The published reference numbers are SIGNAL-level (`pnl = won − ask − fee(ask)` on this
/// row's `ask`): this is the vector comparable to them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheapNpSignal {
    /// Window open (the slug's `sts`), epoch SECONDS.
    pub sts: i64,
    /// The print's own timestamp, epoch MILLISECONDS.
    pub ts: i64,
    /// 0 = Up, 1 = Down.
    pub oidx: u8,
    /// The price the entry was GATED AND PRICED on: the print under [`EntryPrice::Print`], the
    /// VWAP of the resting ask lifted under [`EntryPrice::RestingAsk`].
    pub ask: f64,
    /// The dual-β worst-case edge at [`Self::ask`], i.e. of the trade actually taken
    /// ([`crate::strategies::fair_value::cheap_gate`]'s return under `Print`, re-scored under
    /// `RestingAsk`).
    pub edge: f64,
    /// The tape print that armed the signal; differs from [`Self::ask`] only under
    /// [`EntryPrice::RestingAsk`] (the tape-vs-book gap).
    pub print_px: f64,
    /// Units the entry was sized to: [`CheapNp::size`], capped by the in-edge resting depth under
    /// [`EntryPrice::RestingAsk`].
    pub qty: f64,
    /// `false` for the window's entry, `true` for a later [`CheapNpMode::Flip`] run start.
    pub is_flip: bool,
}

/// Per-window state: the open spot and what we are currently holding.
#[derive(Debug, Clone)]
struct WindowState {
    sts: i64,
    /// Spot at the window open (`price_at(spot, sts)`), `None` until the spot feed is warm.
    s_open: Option<f64>,
    /// Has `s_open` reached its FINAL value? Exactly once a spot sample stamped at or after `sts`
    /// has been seen: later samples are all `> sts` and `price_at` keeps the first among equal
    /// stamps, so it can never change again.
    ///
    /// ⚠ Not a corner case: on the April 2026 tape 8,678 of 8,687 windows print BEFORE their own
    /// open (up to ~a day early), so latching `s_open` on first sight would book a STALE value for
    /// almost every window, and `None` (a dead window) for the 6,206 whose first print precedes
    /// the σ lookback the driver loads spot over.
    s_open_final: bool,
    /// `(outcome_index, symbol, qty)` held in this window, if any. `qty` is the size SUBMITTED —
    /// what a flip must sell — not the broker position, still zero while the entry is in flight.
    held: Option<(u8, String, f64)>,
}

/// The spot-derived pair a print is scored against, memoized for ONE exact `(ts_ms, spot_epoch)`.
///
/// `price_at` and `trailing_sigma` are O(buffer) pure functions of the buffer and the stamp;
/// whole-second prints over a 1 Hz spot feed collapse a window's thousands of prints onto a few
/// hundred points, and without the memo a month re-derives σ ~14 M times.
///
/// ⚠ The key is the EXACT `ts_ms` (never a truncated second) plus [`CheapNp::spot_epoch`]: equal
/// key ⇒ byte-identical inputs ⇒ byte-identical outputs. Exactness-preserving, not approximate.
#[derive(Debug, Clone, Copy)]
struct SpotEval {
    ts_ms: i64,
    epoch: u64,
    s_now: Option<f64>,
    sigma: Option<f64>,
}

/// The `cheap_np` strategy. See the module doc for the rule, the modes and the symbol convention.
pub struct CheapNp {
    /// Symbol of the SPOT reference series (e.g. `"BTCUSDT"`). Never traded; only sampled.
    pub spot_symbol: String,
    /// Hold-to-resolution or flip-on-reversal.
    pub mode: CheapNpMode,
    /// Units bought per entry (the oracle measures 1 share/trade).
    pub size: f64,
    /// Edge bar. Defaults to [`THETA`].
    pub theta: f64,
    /// Window length in seconds used in the probability model. Defaults to [`H`].
    pub h: f64,
    /// σ estimation lookback in seconds. Defaults to [`SIGMA_LOOKBACK_S`].
    pub sigma_lookback_s: f64,
    /// Multiplier on the estimated σ before the gate. `1.0` (the default) is the oracle and must
    /// stay the default — SENSITIVITY ANALYSIS only, never tuning.
    ///
    /// σ is the gate's most assumption-laden input: `trailing_sigma` LINEARLY interpolates gaps
    /// (the oracle's rule), while ClickHouse's `WITH FILL ... INTERPOLATE (px)` carries the last
    /// value and measures a LARGER σ (`σ_linear / σ_locf ≈ 0.9866`, April 2026 `spot_1s`).
    pub sigma_scale: f64,
    /// Only TAKER BUY prints arm the gate (`is_buyer_maker == false`), as the oracle's
    /// `last_trade_price` BUY stream; set `false` for a tape whose maker/taker flag is unpopulated.
    pub taker_buys_only: bool,
    /// Gate on the fired PRINT (default, the published behaviour) or on the RESTING ASK a taker
    /// could actually lift. See [`EntryPrice`].
    pub entry_price: EntryPrice,
    /// Under [`EntryPrice::RestingAsk`], send a marketable LIMIT at the θ-clearing price instead of
    /// a market order (default `true`): under the venue's 250 ms taker delay an order no longer
    /// marketable when matched RESTS rather than filling at whatever the book became
    /// (`vike_model::POLYMARKET_ITODE_HOLD_MS`). Inert under [`EntryPrice::Print`].
    pub limit_at_edge: bool,
    /// Signals refused because the resting ask no longer cleared θ. NOT a smaller trade: no trade.
    pub ask_rejects: usize,
    /// Signals refused because the broker could answer nothing about the book (no `Tick::Book`
    /// series, or a book-less engine). Separate from [`Self::ask_rejects`] so "the data was
    /// missing" can never be read as "the edge was gone".
    pub ask_no_book: usize,
    /// Rolling spot samples `(ts_seconds, price)`, pruned to twice the σ lookback.
    spot: VecDeque<(f64, f64)>,
    /// Bumped on every accepted [`Self::push_spot`] — the invalidation half of [`SpotEval`]'s key.
    /// A counter, because `spot` both grows and is pruned, so its length can repeat.
    spot_epoch: u64,
    /// The last `(s_now, σ)` evaluation. See [`SpotEval`].
    eval: Option<SpotEval>,
    win: Option<WindowState>,
    /// Entries taken (windows in which the gate fired) — an observable for tests/reports.
    pub entries: usize,
    /// Flips taken (`CheapNpMode::Flip` only).
    pub flips: usize,
    /// Every print the gate fired on, in fire order — see [`CheapNpSignal`].
    pub signals: Vec<CheapNpSignal>,
}

impl Default for CheapNp {
    fn default() -> Self {
        CheapNp {
            spot_symbol: String::new(),
            mode: CheapNpMode::Hold,
            size: 1.0,
            theta: THETA,
            h: H,
            sigma_lookback_s: SIGMA_LOOKBACK_S,
            sigma_scale: 1.0,
            taker_buys_only: true,
            entry_price: EntryPrice::Print,
            limit_at_edge: true,
            ask_rejects: 0,
            ask_no_book: 0,
            spot: VecDeque::new(),
            spot_epoch: 0,
            eval: None,
            win: None,
            entries: 0,
            flips: 0,
            signals: Vec::new(),
        }
    }
}

impl CheapNp {
    /// A `Hold` instance sampling `spot_symbol`, 1 unit per entry, oracle defaults everywhere else.
    pub fn new(spot_symbol: impl Into<String>) -> Self {
        CheapNp { spot_symbol: spot_symbol.into(), ..Default::default() }
    }

    /// Read the registry params table — a READER, not a schema (`BuyHold::from_params`
    /// convention): unknown keys are ignored, missing keys take the oracle defaults. Keys: the
    /// fields of the same name, `mode` = `"hold"` | `"flip"`, `entry_price` = `"print"` |
    /// `"resting_ask"` (see [`EntryPrice`]).
    pub fn from_params(params: &toml::Value) -> Self {
        let f = |k: &str| {
            params.get(k).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        };
        let mode = match params.get("mode").and_then(toml::Value::as_str) {
            Some(m) if m.eq_ignore_ascii_case("flip") => CheapNpMode::Flip,
            _ => CheapNpMode::Hold,
        };
        let d = CheapNp::default();
        CheapNp {
            spot_symbol: params
                .get("spot_symbol")
                .and_then(toml::Value::as_str)
                .unwrap_or_default()
                .to_string(),
            mode,
            size: f("size").unwrap_or(d.size),
            theta: f("theta").unwrap_or(d.theta),
            h: f("h").unwrap_or(d.h),
            sigma_lookback_s: f("sigma_lookback_s").unwrap_or(d.sigma_lookback_s),
            sigma_scale: f("sigma_scale").unwrap_or(d.sigma_scale),
            taker_buys_only: params
                .get("taker_buys_only")
                .and_then(toml::Value::as_bool)
                .unwrap_or(d.taker_buys_only),
            // Anything but an explicit resting-ask spelling stays on the frozen PRINT gate: a
            // typo'd profile must not silently change which trades a published run takes.
            entry_price: match params.get("entry_price").and_then(toml::Value::as_str) {
                Some(m)
                    if m.eq_ignore_ascii_case("resting_ask")
                        || m.eq_ignore_ascii_case("resting-ask")
                        || m.eq_ignore_ascii_case("ask") =>
                {
                    EntryPrice::RestingAsk
                }
                _ => EntryPrice::Print,
            },
            limit_at_edge: params
                .get("limit_at_edge")
                .and_then(toml::Value::as_bool)
                .unwrap_or(d.limit_at_edge),
            ..d
        }
    }

    /// Record one spot observation. `ts` is epoch SECONDS (the unit `strategy.py` works in).
    fn push_spot(&mut self, ts: f64, px: f64) {
        // `strategy.py`'s filter is `px > 0`, spelled as a total predicate over f64 (not
        // `!(px > 0.0)`): NaN and non-positive prices are both dropped.
        if px <= 0.0 || px.is_nan() {
            return;
        }
        self.spot.push_back((ts, px));
        self.spot_epoch = self.spot_epoch.wrapping_add(1);
        // Bound the buffer without changing any result: a print evaluates with cutoff
        // `print_ts − lookback` and may lag the newest sample, so keep TWICE the lookback.
        let floor = ts - 2.0 * self.sigma_lookback_s;
        while self.spot.front().is_some_and(|&(t, _)| t < floor) {
            self.spot.pop_front();
        }
    }

    /// Roll to `sts` if it is a new window, and refresh `s_open` (the spot as of the open) on every
    /// print until it is FINAL: a window's tape starts long before its open, so a first-sight latch
    /// would read a stale sample or `None` (see [`WindowState::s_open_final`]).
    fn roll_window(&mut self, sts: i64) {
        if self.win.as_ref().is_none_or(|w| w.sts != sts) {
            self.win = Some(WindowState { sts, s_open: None, s_open_final: false, held: None });
        }
        if self.win.as_ref().is_some_and(|w| w.s_open_final) {
            return;
        }
        let s_open = price_at(self.spot.iter().copied(), sts as f64);
        // FINAL once the feed has crossed the open (see `WindowState::s_open_final`).
        let crossed = self.spot.back().is_some_and(|&(t, _)| t >= sts as f64);
        if let Some(w) = self.win.as_mut() {
            w.s_open = s_open;
            w.s_open_final = crossed;
        }
    }

    /// `(s_now, σ)` at `ts` (epoch seconds, `ts_ms` its exact millisecond stamp), through the
    /// [`SpotEval`] memo, with [`Self::sigma_scale`] applied. Byte-identical to computing both
    /// directly at the default scale of `1.0` (`x * 1.0 == x` exactly for every finite `f64`).
    fn spot_eval(&mut self, ts_ms: i64, ts: f64) -> SpotEval {
        if let Some(e) = self.eval
            && e.ts_ms == ts_ms
            && e.epoch == self.spot_epoch
        {
            return e;
        }
        let e = SpotEval {
            ts_ms,
            epoch: self.spot_epoch,
            s_now: price_at(self.spot.iter().copied(), ts),
            sigma: trailing_sigma(self.spot.iter().copied(), self.sigma_lookback_s, ts)
                .map(|s| s * self.sigma_scale),
        };
        self.eval = Some(e);
        e
    }

    /// The taker-BUY-print handler — the whole strategy.
    fn on_print<B: Broker>(&mut self, broker: &mut B, symbol: &str, ts_ms: i64, ask: f64) {
        let Some(tok) = TokenId::parse(symbol) else { return };
        self.roll_window(tok.sts);
        let Some(win) = self.win.as_ref() else { return };
        let Some(s_open) = win.s_open else { return };

        // In `Hold`, one entry per window and nothing else ever fires again.
        if self.mode == CheapNpMode::Hold && win.held.is_some() {
            return;
        }
        // In `Flip`, a qualifying print on the token we ALREADY hold is not a run start.
        if let Some((held_oidx, _, _)) = win.held.as_ref()
            && *held_oidx == tok.oidx
        {
            return;
        }

        let ts = ts_ms as f64 / 1000.0;
        let t = ts - tok.sts as f64;
        // `cheap_gate`'s two CHEAP predicates, hoisted ahead of the O(buffer) spot work.
        // Behaviour-identical: `cheap_gate` still owns the decision and re-checks them.
        if !in_cheap_band(ask) || !cheap_time_ok(t, self.h) {
            return;
        }
        // σ and s_now are evaluated AT THE PRINT (`cheap_bot.py:66` — "sigma AT THE PRINT (= BT),
        // not the scan moment"), which is what makes live and backtest trade the same windows.
        let e = self.spot_eval(ts_ms, ts);
        let (Some(s_now), Some(sigma)) = (e.s_now, e.sigma) else { return };
        let Some(edge) = cheap_gate(tok.oidx, ask, s_now, s_open, sigma, t, self.theta, self.h)
        else {
            return;
        };

        // The ASK GATE (opt-in; under `EntryPrice::Print` the rest is the frozen path). The print
        // says the market moved, not a price we can obtain: re-score OUR edge at the resting book.
        let (fill_px, qty, limit) = match self.entry_price {
            EntryPrice::Print => (ask, self.size, None),
            EntryPrice::RestingAsk => match self.resolve_ask(broker, symbol, ask, edge) {
                Some(v) => v,
                None => return,
            },
        };

        // Fire. In `Flip` an existing holding is closed first (they are SEPARATE tokens — exiting
        // Up is a sale of the Up token, not a purchase of Down).
        let prior = self.win.as_mut().and_then(|w| w.held.take());
        let is_flip = prior.is_some();
        if let Some((_, held_sym, held_qty)) = prior {
            if held_qty > 0.0 {
                broker.submit_market(&held_sym, -1, held_qty);
            }
            self.flips += 1;
        } else {
            self.entries += 1;
        }
        self.signals.push(CheapNpSignal {
            sts: tok.sts,
            ts: ts_ms,
            oidx: tok.oidx,
            ask: fill_px,
            edge: edge_at(prob_wc(ask, edge), fill_px),
            print_px: ask,
            qty,
            is_flip,
        });
        match limit {
            // Marketable LIMIT at the θ-clearing price: the ONLY order shape well-defined under the
            // venue's 250 ms hold — still marketable, it fills; market ran away, it RESTS.
            Some(px) => broker.submit_limit(symbol, 1, qty, px),
            None => broker.submit_market(symbol, 1, qty),
        }
        if let Some(w) = self.win.as_mut() {
            w.held = Some((tok.oidx, symbol.to_string(), qty));
        }
    }

    /// Steps 2–5 of the resting-ask rule: read the book through the broker, re-score the edge at
    /// the obtainable price, size to the in-edge depth. `(fill_price, qty, limit)`, or `None` = no
    /// trade (a signal whose edge does not survive the real ask is not a smaller trade).
    ///
    /// Book walk, delay and taker price are ENGINE properties
    /// (`vike_fills::fill_model::L2BookFillModel`, [`Broker::quote_vwap`],
    /// `vike_model::POLYMARKET_ITODE_HOLD_MS`); the only strategy knowledge here is "re-check θ at
    /// the price I would really pay".
    fn resolve_ask<B: Broker>(
        &mut self,
        broker: &B,
        symbol: &str,
        ask: f64,
        edge: f64,
    ) -> Option<(f64, f64, Option<f64>)> {
        let pw = prob_wc(ask, edge);
        // The worst price still worth paying. `None` (even a free fill misses θ) cannot happen for
        // a signal that just fired, but is refused rather than assumed away.
        let Some(limit) = price_at_edge(pw, self.theta) else {
            self.ask_rejects += 1;
            return None;
        };
        // ANY resting ask at all? The infinitesimal probe succeeds iff one ask level exists, which
        // separates "no book fed" (a DATA problem) from "nothing worth lifting" (a real refusal).
        if broker.quote_vwap(symbol, 1, BOOK_PROBE_QTY).is_none() {
            self.ask_no_book += 1;
            return None;
        }
        // Size to the depth inside that limit — never to the request.
        let avail = broker.depth_within_price(symbol, 1, limit);
        if avail <= 0.0 {
            self.ask_rejects += 1;
            return None;
        }
        let qty = self.size.min(avail);
        let Some(vwap) = broker.quote_vwap(symbol, 1, qty) else {
            self.ask_no_book += 1;
            return None;
        };
        // THE decision. STRICTLY above the bar, matching `cheap_gate`'s own `>`; a NaN edge (an
        // impossible book) refuses rather than comparing false into an entry.
        let obtainable_edge = edge_at(pw, vwap);
        if obtainable_edge.is_nan() || obtainable_edge <= self.theta {
            self.ask_rejects += 1;
            return None;
        }
        Some((vwap, qty, self.limit_at_edge.then_some(limit)))
    }
}

impl<B: Broker> Strategy<B> for CheapNp {
    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {
        if t.symbol == self.spot_symbol {
            self.push_spot(t.ts as f64 / 1000.0, t.price);
            return;
        }
        // A taker BUY has `is_buyer_maker == false` (the buyer crossed the spread).
        if self.taker_buys_only && t.is_buyer_maker {
            return;
        }
        self.on_print(broker, &t.symbol, t.ts, t.price);
    }

    fn on_quote_tick(&mut self, broker: &mut B, q: &QuoteTick) {
        // Quotes only feed the SPOT series: the token side is tape-driven by design.
        let _ = broker;
        if q.symbol == self.spot_symbol {
            let mid = match (q.bid > 0.0, q.ask > 0.0) {
                (true, true) => 0.5 * (q.bid + q.ask),
                (true, false) => q.bid,
                (false, true) => q.ask,
                (false, false) => return,
            };
            self.push_spot(q.ts as f64 / 1000.0, mid);
        }
    }

    fn on_bar(&mut self, broker: &mut B, bar: &Bar) {
        let _ = broker;
        if bar.symbol.as_deref() == Some(self.spot_symbol.as_str()) {
            self.push_spot(bar.ts as f64 / 1000.0, bar.close);
        }
    }
}
