//! `cheap_np` — the Polymarket BTC 5-minute up/down fair-value mispricing TAKER.
//!
//! Port of the live bot `vike_db_data_jobs/trading/fair_value_bot/cheap_bot.py` (the oracle;
//! read-only), whose entry rule is `FairValueCheapBot._cheap_tape_entry` and whose math is
//! [`crate::fair_value`] (a port of that package's `strategy.py`). The `_np` suffix is the live
//! recording tag (`cheap_bot.py:_rec_leg`) for the **N**o-**P**ersistence variant: the cheap leg
//! fires on the now-edge alone, with no 5-second confirmation.
//!
//! # The rule
//!
//! Each 5-minute window is a pair of separate ERC-1155 outcome tokens (`outcome_index` 0 = Up,
//! 1 = Down) whose prices sum to $1. The strategy watches the **taker BUY tape** of both tokens and
//! enters on the FIRST print that clears the whole gate — cheap band `[0.10, 0.35)`, time-till-
//! expiry in `[15, 270]` s, and a dual-β worst-case edge strictly above θ = 0.055 evaluated at
//! THAT PRINT's own spot and time (not the scan moment) — then holds to resolution.
//!
//! It is deliberately TAPE-driven, not book-driven (`cheap_bot.py:_find_entry`): the favourable ask
//! is usually taken before it ever rests in the book, so polling the resting book enters windows the
//! backtest never saw and misses the ones it did.
//!
//! # Modes
//!
//! * [`CheapNpMode::Hold`] — the live `cheap_np` behaviour and what the reference numbers encode:
//!   one entry per window, held to resolution. Settlement is the ENGINE's job
//!   (`vike_backtest::EngineParams::resolution` + `SimBroker::settle_at_payout`), so this strategy never
//!   submits an exit.
//! * [`CheapNpMode::Flip`] — momentum-follow: a later qualifying print on the OPPOSITE token exits
//!   the held one and enters the other. Mirrors the backtest `trading/fair_value/flip_cheap.py`,
//!   whose `gate_cheap` restricts the qualifying stream first and then takes the **run starts** —
//!   rows where `outcome_index` differs from the previous qualifying row of the same window. Entry
//!   is run-start #0; every later run start is a flip. Multi-flip, like `cheap_bot.py`.
//!
//! # Symbol convention (why the strategy parses its own symbols)
//!
//! There is no instrument expiry/activation anywhere in the model layer (see the port backlog's
//! G9), so the window's open `sts` — and therefore `t` and the token pairing — must come from the
//! market's slug, exactly as the Python does. One `CheapNp` instance spans thousands of windows, so
//! the window identity has to be carried IN the symbol rather than configured:
//!
//! ```text
//! btc-updown-5m-1772323200#0     <slug>#<outcome_index>
//!                ^^^^^^^^^^ ^    sts (must be a multiple of 300 — the Python's own guard)
//! ```
//!
//! Anything that does not parse is ignored (never traded), which also satisfies the backlog's G5
//! concern: the spot reference series can be handed to the engine as a symbol and this strategy will
//! never submit an order against it.
//!
//! # Feed ordering
//!
//! Every decision is derived from the tick payload itself — the print's price, its `ts`, and the
//! spot samples the strategy has seen SO FAR — so the strategy is correct under both replay
//! orderings (`TickReplayConfig::feed_latency` false = venue clock, true = arrival clock). Under
//! arrival ordering a print simply arrives later relative to spot, which is precisely the latency
//! haircut being measured; nothing here reads a future sample. The one ordering-sensitive rule is
//! "first qualifying print per window", which is resolved in ARRIVAL order by construction — the
//! honest live semantic.

use std::collections::VecDeque;

use vike_model::fair::UPDOWN_WINDOW_SECS;
use vike_model::{Bar, Broker, QuoteTick, Strategy, TradeTick};

use crate::cheap_np_ask::{edge_at, price_at_edge, prob_wc};
use crate::fair_value::{
    H, SIGMA_LOOKBACK_S, THETA, cheap_gate, cheap_time_ok, in_cheap_band, price_at, trailing_sigma,
};

/// The infinitesimal quantity [`CheapNp::resolve_ask`] probes the book with to tell "no book" from
/// "no liquidity worth lifting". Any positive value works (a book walk takes `min(level, need)`),
/// so this is a sentinel, not a size — it is never ordered.
const BOOK_PROBE_QTY: f64 = 1e-9;

/// WHICH PRICE the entry is gated and priced on — the fix for the tape-print entry flaw.
///
/// See [`crate::cheap_np_ask`] for the measurement and the layering. In one line: the print is
/// what somebody else already took, so gating on it prices entries a taker cannot obtain, and —
/// worse — scores their edge 4–8 cents too generously against a 5.5 cent bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EntryPrice {
    /// The fired print's own price (the live `cheap_np` behaviour and what every published
    /// reference number encodes). THE DEFAULT, so a default `CheapNp` is byte-identical to the
    /// pre-ask-gate strategy and the 12,639-entry parity gate is untouched.
    #[default]
    Print,
    /// The RESTING ASK, read from the broker's book. The print becomes a SIGNAL ONLY ("the market
    /// moved"); the edge is re-scored at the price actually obtainable and the entry is taken only
    /// if it STILL clears θ, sized to the depth that stays within edge.
    ///
    /// Requires a broker that can answer [`Broker::quote_vwap`] / [`Broker::depth_within_price`] —
    /// i.e. a `run_ticks` replay carrying `Tick::Book` events. On a broker that cannot (the
    /// default `None`/`0.0` answers), every signal is refused and counted in
    /// [`CheapNp::ask_no_book`] rather than silently falling back to the print price: a run that
    /// forgot to feed the book must look like zero entries, never like a good backtest.
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
    /// Parse `"btc-updown-5m-1772323200#0"`. Returns `None` unless the symbol has the `#<0|1>`
    /// suffix, a trailing `-<integer>` slug segment, and an `sts` that is a whole multiple of
    /// [`UPDOWN_WINDOW_SECS`] — the same `sts % 300 == 0` guard the Python applies (`bot.py:_roll_window`
    /// derives `sts` by flooring, so a slug that is not on the grid is not a 5-minute window at all).
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

/// One print the gate fired on, recorded as it fires — the SIGNAL, before the engine executes it.
///
/// Kept because the executed number and the signalled number are two different quantities and a
/// report that conflates them hides the interesting part: the engine fills a market order at the
/// NEXT print of that token, so the price actually paid is not `ask` here, and a token whose next
/// print never arrives before resolution fills at nothing at all. The published `cheap_np`
/// reference numbers are SIGNAL-level (`pnl = won − ask − fee(ask)` on this row's own `ask`), so
/// this is the vector that is directly comparable to them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CheapNpSignal {
    /// Window open (the slug's `sts`), epoch SECONDS.
    pub sts: i64,
    /// The print's own timestamp, epoch MILLISECONDS.
    pub ts: i64,
    /// 0 = Up, 1 = Down.
    pub oidx: u8,
    /// The price the entry was GATED AND PRICED on. Under [`EntryPrice::Print`] (the default) this
    /// is the print's own price, exactly as before. Under [`EntryPrice::RestingAsk`] it is the
    /// VWAP of the resting ask the strategy would actually have lifted.
    pub ask: f64,
    /// The dual-β worst-case edge at [`Self::ask`] — [`crate::fair_value::cheap_gate`]'s own
    /// return under [`EntryPrice::Print`], and the RE-SCORED edge at the obtainable price under
    /// [`EntryPrice::RestingAsk`]. Either way it is the edge of the trade actually taken.
    pub edge: f64,
    /// The tape print that armed the signal. Equal to [`Self::ask`] under [`EntryPrice::Print`];
    /// under [`EntryPrice::RestingAsk`] the two differ by the tape-vs-book gap this whole change
    /// exists to stop hiding.
    pub print_px: f64,
    /// Units the entry was sized to. [`CheapNp::size`] under [`EntryPrice::Print`]; under
    /// [`EntryPrice::RestingAsk`] it is capped by the in-edge depth actually resting.
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
    /// Has `s_open` reached its FINAL value? It has, exactly once a spot sample stamped at or
    /// after `sts` has been observed — from then on `price_at(spot, sts)` can never change (later
    /// samples are all `> sts`, and `price_at` keeps the first among equal stamps).
    ///
    /// This flag is the whole reason `s_open` is not simply latched on first sight of the window,
    /// and it is not a corner case: on the real April 2026 tape **8,678 of 8,687** windows have at
    /// least one print BEFORE their own open (the earliest lands `t = −85,725` s, nearly a day
    /// early — the module doc's old "roughly [−500, +370] s" reading was measured on a narrower
    /// sample and does not hold). Latching at the first print would therefore have booked a STALE
    /// `s_open` for essentially every window, and — for the 6,206 windows whose first print
    /// precedes the σ lookback the driver loads spot over — would have latched `None` and killed
    /// the window outright.
    s_open_final: bool,
    /// `(outcome_index, symbol, qty)` of the token currently held in this window, if any. `qty` is
    /// the size SUBMITTED, which is what a flip must sell — not the broker's position read, which
    /// is still zero while the entry order is in flight (a market order fills at the next print of
    /// its own symbol, and a thin token can go a long time between prints).
    held: Option<(u8, String, f64)>,
}

/// The spot-derived pair a print is scored against, memoized for ONE exact `(ts_ms, spot_epoch)`.
///
/// Both `price_at` and `trailing_sigma` are pure functions of the spot buffer and the evaluation
/// stamp, and both are O(buffer) — `trailing_sigma` builds a 1,800-entry map and two vectors every
/// call. The real tape prints at WHOLE-SECOND resolution and the spot feed is 1 Hz, so a window's
/// thousands of prints collapse onto a few hundred distinct evaluation points; without this memo a
/// month of `btc-updown-5m` re-derives σ ~14 M times and the run does not finish in useful time.
///
/// The key is the EXACT `ts_ms` (never a truncated second) paired with [`CheapNp::spot_epoch`], a
/// counter bumped on every accepted spot sample. Equal key ⇒ byte-identical inputs ⇒ byte-identical
/// outputs, on any tape and under any interleaving — the memo is an exactness-preserving cache, not
/// an approximation.
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
    /// Multiplier applied to the estimated σ before it reaches the gate. `1.0` (the default) is
    /// the oracle and must stay the default — this knob exists for SENSITIVITY ANALYSIS, not for
    /// tuning.
    ///
    /// σ is the single most assumption-laden input in the gate, because the spot feed is ~12 %
    /// incomplete and the estimator's gap policy decides what σ even means. `trailing_sigma`
    /// LINEARLY interpolates missing seconds (the live oracle's rule, bit-parity-tested); a
    /// replication that carries the last value forward instead — what ClickHouse's
    /// `WITH FILL ... INTERPOLATE (px)` actually does — measures a systematically LARGER σ
    /// (`σ_linear / σ_locf ≈ 0.9866` over April 2026's `spot_1s`, measured on the latency box). Being able
    /// to re-run the whole month at a scaled σ is what turns "the two answers differ" into a
    /// quantified attribution.
    pub sigma_scale: f64,
    /// Only TAKER BUY prints arm the gate (a taker buy has `is_buyer_maker == false`). The oracle
    /// reads Polymarket's `last_trade_price` BUY stream, so this defaults to `true`; set it `false`
    /// for a tape whose maker/taker flag is not populated.
    pub taker_buys_only: bool,
    /// Gate on the fired PRINT (default, byte-identical to the published behaviour) or on the
    /// RESTING ASK a taker could actually lift. See [`EntryPrice`].
    pub entry_price: EntryPrice,
    /// Under [`EntryPrice::RestingAsk`], send a marketable LIMIT at the θ-clearing price instead of
    /// a market order. `true` (the default for that mode) is what makes the venue's 250 ms taker
    /// delay expressible: an order that is marketable when the strategy decides and no longer
    /// marketable when the matching engine looks at it RESTS rather than filling at whatever the
    /// book has become (see `vike_backtest::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS`). Inert under
    /// [`EntryPrice::Print`], which always submits the frozen market order.
    pub limit_at_edge: bool,
    /// Signals refused because the resting ask no longer cleared θ — the headline number of the
    /// ask-gate change. A signal counted here is NOT a smaller trade, it is not a trade.
    pub ask_rejects: usize,
    /// Signals refused because the broker could answer nothing about the book (no `Tick::Book`
    /// series for that token, or a book-less engine). Counted separately from
    /// [`Self::ask_rejects`] so "the data was missing" can never be read as "the edge was gone".
    pub ask_no_book: usize,
    /// Rolling spot samples `(ts_seconds, price)`, pruned to twice the σ lookback.
    spot: VecDeque<(f64, f64)>,
    /// Bumped on every accepted [`Self::push_spot`] — the memo-invalidation half of [`SpotEval`]'s
    /// key. A counter rather than a buffer fingerprint because `spot` both grows and is pruned,
    /// so its length alone can repeat across genuinely different contents.
    spot_epoch: u64,
    /// The last `(s_now, σ)` evaluation, reused while the print stamp AND the spot buffer are both
    /// unchanged. See [`SpotEval`].
    eval: Option<SpotEval>,
    win: Option<WindowState>,
    /// Entries taken (windows in which the gate fired) — an observable for tests/reports.
    pub entries: usize,
    /// Flips taken (`CheapNpMode::Flip` only).
    pub flips: usize,
    /// Every print the gate fired on, in fire order — see [`CheapNpSignal`]. Appended to
    /// unconditionally; entries are rare by construction (at most one per window in `Hold`), so
    /// this costs one push per fired gate and nothing at all on the reject path.
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

    /// Read the registry params table (`BuyHold::from_params` convention — a READER, not a schema:
    /// unknown keys are ignored, missing keys fall back to the oracle defaults).
    ///
    /// | key | meaning | default |
    /// |---|---|---|
    /// | `spot_symbol` | the reference spot series (never traded) | `""` |
    /// | `mode` | `"hold"` \| `"flip"` | `"hold"` |
    /// | `size` | units per entry | `1.0` |
    /// | `theta` | edge bar | `0.055` |
    /// | `h` | window length, seconds | `300.0` |
    /// | `sigma_lookback_s` | σ lookback, seconds | `1800.0` |
    /// | `sigma_scale` | σ multiplier — SENSITIVITY ONLY, never tuning | `1.0` |
    /// | `taker_buys_only` | require `is_buyer_maker == false` | `true` |
    /// | `entry_price` | `"print"` \| `"resting_ask"` (see [`EntryPrice`]) | `"print"` |
    /// | `limit_at_edge` | send a marketable limit at the θ-clearing price | `true` |
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
        // `strategy.py`'s own filter is `px > 0`; spelled out this way (rather than `!(px > 0.0)`)
        // so it reads as a total predicate over f64 — NaN and non-positive prices are both dropped.
        if px <= 0.0 || px.is_nan() {
            return;
        }
        self.spot.push_back((ts, px));
        self.spot_epoch = self.spot_epoch.wrapping_add(1);
        // Bound the buffer without changing any result: a print is only ever evaluated with a
        // cutoff of `print_ts − lookback`, and a print may lag the newest spot sample, so keep
        // TWICE the lookback rather than exactly one.
        let floor = ts - 2.0 * self.sigma_lookback_s;
        while self.spot.front().is_some_and(|&(t, _)| t < floor) {
            self.spot.pop_front();
        }
    }

    /// Roll to `sts` if it is a new window, and (re)derive `s_open` = the spot as of the window
    /// open until it is FINAL.
    ///
    /// `s_open` cannot simply be captured on first sight of the window: a window's tape routinely
    /// starts long before its own open, and `price_at` over a buffer that has not yet reached
    /// `sts` answers with a stale sample — or, when the buffer is still empty, with `None`. So the
    /// value is refreshed on every print until a spot sample at or after `sts` has been observed,
    /// at which point it is provably fixed and is latched (see [`WindowState::s_open_final`] for
    /// how much of the real tape this covers).
    fn roll_window(&mut self, sts: i64) {
        if self.win.as_ref().is_none_or(|w| w.sts != sts) {
            self.win = Some(WindowState { sts, s_open: None, s_open_final: false, held: None });
        }
        if self.win.as_ref().is_some_and(|w| w.s_open_final) {
            return;
        }
        let s_open = price_at(self.spot.iter().copied(), sts as f64);
        // FINAL once the feed has crossed the open — every later sample is `> sts` and
        // `price_at` keeps the first among equal stamps, so the answer can no longer move.
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
        // The two CHEAP predicates of `cheap_gate`, hoisted ahead of the O(buffer) spot work.
        // Behaviour-identical — `cheap_gate` still owns the decision and re-checks them below;
        // this only stops ~85 % of the real tape from paying for a σ it can never use.
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

        // The ASK GATE (opt-in — `EntryPrice::Print` skips this entirely and the rest of the
        // function is the frozen path). The print has told us the market moved; it has NOT told us
        // a price we can obtain. Ask the broker what the resting book would actually charge, and
        // re-score OUR OWN edge there.
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
            // Marketable LIMIT at the θ-clearing price: the ONLY order shape whose outcome under
            // the venue's 250 ms hold is well-defined. If it is still marketable when the matching
            // engine looks, it fills; if the market ran away inside the hold, it RESTS — which is
            // what the venue does, and is neither a fill nor a miss.
            Some(px) => broker.submit_limit(symbol, 1, qty, px),
            None => broker.submit_market(symbol, 1, qty),
        }
        if let Some(w) = self.win.as_mut() {
            w.held = Some((tok.oidx, symbol.to_string(), qty));
        }
    }

    /// Steps 2–5 of the resting-ask rule: read the book through the broker, re-score the edge at
    /// the price actually obtainable, and size to the depth that stays within edge.
    ///
    /// Returns `(fill_price, qty, limit)` or `None` when there is no trade — which is the WHOLE
    /// point: a signal whose edge does not survive the real ask is not a smaller trade.
    ///
    /// Note what this function does NOT do. It does not walk a book, model a delay, or decide what
    /// a taker pays — those are engine properties that serve every strategy
    /// ([`crate::fill_model::L2BookFillModel`], [`Broker::quote_vwap`],
    /// `vike_backtest::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS`). The only strategy-specific knowledge
    /// here is "θ is my bar, and I must re-check it at the price I would really pay".
    fn resolve_ask<B: Broker>(
        &mut self,
        broker: &B,
        symbol: &str,
        ask: f64,
        edge: f64,
    ) -> Option<(f64, f64, Option<f64>)> {
        let pw = prob_wc(ask, edge);
        // The worst price still worth paying. `None` ⇒ even a free fill misses θ, which cannot
        // happen for a signal that just fired, but is refused rather than assumed away.
        let Some(limit) = price_at_edge(pw, self.theta) else {
            self.ask_rejects += 1;
            return None;
        };
        // Is there ANY resting ask at all? A book walk takes `min(level, need)`, so an
        // infinitesimal probe succeeds iff at least one ask level exists — which separates "no
        // book was fed / the ask side is blank" (a DATA problem, never a strategy result) from "the
        // book is there and has nothing worth lifting" (a real refusal).
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
        // Quotes only ever feed the SPOT series (the token side is tape-driven by design — see the
        // module doc's "tape-driven, not book-driven"). `_` binds the broker so the signature stays
        // the trait's.
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
