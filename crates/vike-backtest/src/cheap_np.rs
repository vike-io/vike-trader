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
//!   ([`crate::EngineParams::resolution`] + `SimBroker::settle_at_payout`), so this strategy never
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

use vike_model::{Bar, Broker, QuoteTick, Strategy, TradeTick};

use crate::cheap_np_ask::{edge_at, price_at_edge, prob_wc};
use crate::fair_value::{
    H, SIGMA_LOOKBACK_S, THETA, cheap_gate, cheap_time_ok, in_cheap_band, price_at, trailing_sigma,
};

/// Window length in seconds — and the modulus the slug's `sts` must be a multiple of.
pub const WINDOW_SECS: i64 = 300;

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
    /// [`WINDOW_SECS`] — the same `sts % 300 == 0` guard the Python applies (`bot.py:_roll_window`
    /// derives `sts` by flooring, so a slug that is not on the grid is not a 5-minute window at all).
    pub fn parse(symbol: &str) -> Option<TokenId> {
        let (slug, idx) = symbol.rsplit_once('#')?;
        let oidx: u8 = idx.parse().ok()?;
        if oidx > 1 {
            return None;
        }
        let (_, sts_str) = slug.rsplit_once('-')?;
        let sts: i64 = sts_str.parse().ok()?;
        (sts > 0 && sts % WINDOW_SECS == 0).then_some(TokenId { sts, oidx })
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
    /// book has become (see [`crate::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS`]). Inert under
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
    /// [`crate::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS`]). The only strategy-specific knowledge
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{EngineParams, StrategyEngine, Tick};
    use crate::fair_value::wc;

    const SPOT: &str = "BTCUSDT";
    const STS: i64 = 1_772_323_200;

    fn up(sts: i64) -> String {
        format!("btc-updown-5m-{sts}#0")
    }
    fn dn(sts: i64) -> String {
        format!("btc-updown-5m-{sts}#1")
    }

    #[test]
    fn token_symbol_parses_slug_and_outcome() {
        assert_eq!(TokenId::parse(&up(STS)), Some(TokenId { sts: STS, oidx: 0 }));
        assert_eq!(TokenId::parse(&dn(STS)), Some(TokenId { sts: STS, oidx: 1 }));
    }

    #[test]
    fn token_symbol_rejects_off_grid_and_malformed() {
        // sts % 300 != 0 -> not a 5-minute window (the Python's own guard)
        assert_eq!(TokenId::parse("btc-updown-5m-1772323201#0"), None);
        assert_eq!(TokenId::parse("btc-updown-5m-1772323200#2"), None, "outcome must be 0|1");
        assert_eq!(TokenId::parse("btc-updown-5m-1772323200"), None, "no outcome suffix");
        assert_eq!(TokenId::parse("BTCUSDT"), None, "the spot series is never a token");
        assert_eq!(TokenId::parse("btc-updown-5m-abc#0"), None);
        assert_eq!(TokenId::parse("btc-updown-5m-0#0"), None, "sts must be positive");
    }

    /// A spot series: `n` one-second samples from `sts - warm` seconds, drifting up by `bp` basis
    /// points per second so `p_up` is meaningfully above 0.5 and σ is non-degenerate.
    fn spot_ticks(from_s: i64, n: i64, start: f64, bp: f64) -> Vec<Tick> {
        (0..n)
            .map(|i| {
                let px = start * (1.0 + bp * 1e-4 * i as f64) + if i % 2 == 0 { 0.5 } else { 0.0 };
                Tick::Trade(TradeTick {
                    ts: (from_s + i) * 1000,
                    local_ts: 0,
                    price: px,
                    size: 1.0,
                    is_buyer_maker: false,
                    symbol: SPOT.to_string(),
                })
            })
            .collect()
    }

    fn print_tick(sym: &str, ts_s: i64, price: f64, buyer_maker: bool) -> Tick {
        Tick::Trade(TradeTick {
            ts: ts_s * 1000,
            local_ts: 0,
            price,
            size: 1.0,
            is_buyer_maker: buyer_maker,
            symbol: sym.to_string(),
        })
    }

    fn run(strat: CheapNp, series: Vec<(String, Vec<Tick>)>) -> StrategyEngine<CheapNp> {
        run_with(strat, series, EngineParams { cash: 1000.0, ..Default::default() })
    }

    fn run_with(
        strat: CheapNp,
        series: Vec<(String, Vec<Tick>)>,
        params: EngineParams,
    ) -> StrategyEngine<CheapNp> {
        let symbols: Vec<(String, Vec<Bar>)> =
            series.iter().map(|(s, _)| (s.clone(), Vec::new())).collect();
        let mut e = StrategyEngine::new(symbols, strat, params);
        e.run_ticks(&series);
        e
    }

    /// A full-book SNAPSHOT tick for `sym` at `ts_s`, with the given ask ladder.
    fn book_tick(sym: &str, ts_s: i64, asks: &[(f64, f64)]) -> Tick {
        Tick::Book(vike_model::BookUpdate {
            ts: ts_s * 1000,
            local_ts: ts_s * 1000,
            seq: ts_s as u64,
            kind: vike_model::BookUpdateKind::Snapshot,
            tick_size: 0.001,
            bids: vec![(0.05, 1_000.0)],
            asks: asks.to_vec(),
            symbol: sym.to_string(),
        })
    }

    /// The `enters_on_the_first_qualifying_print_and_holds` fixture, plus an ask ladder resting on
    /// the UP token from `STS + 20` onward. One print at `t = 40` clears the gate at 0.20.
    fn with_book(asks: &[(f64, f64)]) -> Vec<(String, Vec<Tick>)> {
        let mut series = with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4);
        // The book rides the UP token's own stream, so the k-way merge delivers it before the
        // same-second print (equal ts breaks by stream order, and this entry precedes the print).
        let ups = series.iter_mut().find(|(s, _)| *s == up(STS)).expect("the up stream");
        ups.1.insert(0, book_tick(&up(STS), STS + 20, asks));
        series
    }

    /// 90 seconds of spot warmup ending at the window open, then the window's prints.
    fn with_warm_spot(prints: Vec<Tick>, drift_bp: f64) -> Vec<(String, Vec<Tick>)> {
        // 200 samples of spot: 110 before the open (warmup + s_open) and 90 into the window
        let spot = spot_ticks(STS - 110, 200, 60_000.0, drift_bp);
        let mut by: Vec<(String, Vec<Tick>)> = vec![(SPOT.to_string(), spot)];
        let mut up_p = Vec::new();
        let mut dn_p = Vec::new();
        for t in prints {
            let Tick::Trade(tt) = &t else { unreachable!() };
            if tt.symbol.ends_with("#0") {
                up_p.push(t);
            } else {
                dn_p.push(t);
            }
        }
        by.push((up(STS), up_p));
        by.push((dn(STS), dn_p));
        by
    }

    #[test]
    fn enters_on_the_first_qualifying_print_and_holds() {
        // Up-drifting spot: the UP token at 0.20 is mispriced -> the gate fires.
        let prints = vec![
            // t = 10 -> tte 290, OUTSIDE the time window even though the price qualifies
            print_tick(&up(STS), STS + 10, 0.20, false),
            // t = 40 -> the first print that clears band + time + theta
            print_tick(&up(STS), STS + 40, 0.20, false),
            // later qualifying prints must NOT re-enter (Hold)
            print_tick(&up(STS), STS + 60, 0.20, false),
            print_tick(&up(STS), STS + 80, 0.20, false),
        ];
        let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
        assert_eq!(e.strategy.entries, 1, "exactly one entry per window");
        assert_eq!(e.strategy.flips, 0);
        assert_eq!(e.core.position_of(&up(STS)).size, 1.0, "1 unit, held");
        assert_eq!(e.core.position_of(&dn(STS)).size, 0.0);
        assert_eq!(e.core.position_of(SPOT).size, 0.0, "the spot series is NEVER traded");
    }

    /// A window's tape starting BEFORE its own open must not corrupt (or kill) `s_open`.
    ///
    /// This is not a synthetic corner: on the real April 2026 tape 8,678 of 8,687 windows print
    /// before their open, and 6,206 of them print before the σ lookback the driver loads spot
    /// over. The proof shape is a differential one — the same qualifying print must produce a
    /// BYTE-IDENTICAL signal whether or not a pre-open print preceded it.
    #[test]
    fn a_pre_open_print_leaves_s_open_untouched() {
        let entry = print_tick(&up(STS), STS + 40, 0.20, false);
        let baseline = run(CheapNp::new(SPOT), with_warm_spot(vec![entry.clone()], 0.4));
        assert_eq!(baseline.strategy.entries, 1, "control: the entry fires without a lead-in");

        // (a) a print while the spot buffer has samples but has NOT yet reached the open —
        //     latching here would book a STALE `s_open` (the spot 50 s before the open).
        let stale = run(
            CheapNp::new(SPOT),
            with_warm_spot(vec![print_tick(&up(STS), STS - 50, 0.20, false), entry.clone()], 0.4),
        );
        // (b) a print before the spot feed exists at all — latching here would book `None` and
        //     the window could never trade again.
        let empty = run(
            CheapNp::new(SPOT),
            with_warm_spot(vec![print_tick(&up(STS), STS - 200, 0.20, false), entry], 0.4),
        );

        for (label, e) in [("stale-buffer lead-in", &stale), ("empty-buffer lead-in", &empty)] {
            assert_eq!(e.strategy.entries, 1, "{label}: the window must still enter");
            assert_eq!(
                e.strategy.signals, baseline.strategy.signals,
                "{label}: the fired signal must be identical to the no-lead-in control"
            );
        }
    }

    #[test]
    fn out_of_band_and_out_of_time_prints_never_enter() {
        let prints = vec![
            print_tick(&up(STS), STS + 40, 0.40, false), // above the band
            print_tick(&up(STS), STS + 50, 0.05, false), // below the band
            print_tick(&up(STS), STS + 10, 0.20, false), // tte 290 > 270
            print_tick(&up(STS), STS + 290, 0.20, false), // tte 10 < 15
        ];
        let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
        assert_eq!(e.strategy.entries, 0);
    }

    #[test]
    fn the_wrong_side_never_qualifies_on_an_up_move() {
        // Spot drifting UP: the DOWN token at 0.20 has a deeply negative edge.
        let prints = vec![print_tick(&dn(STS), STS + 40, 0.20, false)];
        let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
        assert_eq!(e.strategy.entries, 0);
        assert_eq!(e.core.position_of(&dn(STS)).size, 0.0);
    }

    #[test]
    fn maker_side_prints_are_ignored_when_taker_buys_only() {
        let prints = vec![print_tick(&up(STS), STS + 40, 0.20, true)];
        let e = run(CheapNp::new(SPOT), with_warm_spot(prints.clone(), 0.4));
        assert_eq!(e.strategy.entries, 0, "is_buyer_maker = a taker SELL, not a taker buy");

        let mut s = CheapNp::new(SPOT);
        s.taker_buys_only = false;
        let e = run(s, with_warm_spot(prints, 0.4));
        assert_eq!(e.strategy.entries, 1, "flag off -> the same print enters");
    }

    #[test]
    fn no_entry_before_sigma_is_warm() {
        // Only 20 spot seconds observed -> trailing_sigma is None (warmup floor 30) -> no gate.
        let spot = spot_ticks(STS - 10, 20, 60_000.0, 0.4);
        let series = vec![
            (SPOT.to_string(), spot),
            (up(STS), vec![print_tick(&up(STS), STS + 40, 0.20, false)]),
        ];
        let e = run(CheapNp::new(SPOT), series);
        assert_eq!(e.strategy.entries, 0);
    }

    #[test]
    fn no_entry_without_a_window_open_price() {
        // Spot starts AFTER the window open -> price_at(spot, sts) is None -> s_open unknown.
        let spot = spot_ticks(STS + 5, 120, 60_000.0, 0.4);
        let series = vec![
            (SPOT.to_string(), spot),
            (up(STS), vec![print_tick(&up(STS), STS + 100, 0.20, false)]),
        ];
        let e = run(CheapNp::new(SPOT), series);
        assert_eq!(e.strategy.entries, 0);
    }

    #[test]
    fn each_window_gets_its_own_entry() {
        let spot = spot_ticks(STS - 110, 800, 60_000.0, 0.4);
        let mut series = vec![(SPOT.to_string(), spot)];
        for w in 0..2i64 {
            let sts = STS + w * WINDOW_SECS;
            series.push((up(sts), vec![print_tick(&up(sts), sts + 40, 0.20, false)]));
        }
        let e = run(CheapNp::new(SPOT), series);
        assert_eq!(e.strategy.entries, 2, "one entry per WINDOW, not per run");
    }

    #[test]
    fn flip_mode_exits_the_held_token_and_enters_the_opposite() {
        // Spot drifts UP for the first half of the window (Up qualifies), then reverses hard so the
        // DOWN token qualifies later. Built as an explicit spot path rather than a constant drift.
        let mut spot: Vec<Tick> = Vec::new();
        for i in 0..260i64 {
            let s = STS - 110 + i;
            // up to +60 bp by t=+40, then back down through the open by t=+120
            let rel = (s - STS) as f64;
            let px = if rel <= 40.0 {
                60_000.0 * (1.0 + 1.5e-5 * rel.max(0.0))
            } else {
                60_000.0 * (1.0 + 1.5e-5 * 40.0 - 4.0e-5 * (rel - 40.0))
            };
            spot.push(Tick::Trade(TradeTick {
                ts: s * 1000,
                local_ts: 0,
                price: px + if i % 2 == 0 { 0.5 } else { 0.0 },
                size: 1.0,
                is_buyer_maker: false,
                symbol: SPOT.to_string(),
            }));
        }
        // Filler prints at 0.50 (OUT of the cheap band, so they can never arm the gate) exist only
        // so the market orders have a later print of their own symbol to fill against — the sim
        // engine fills a market order at the NEXT event of that symbol.
        let series = vec![
            (SPOT.to_string(), spot.clone()),
            (
                up(STS),
                vec![
                    print_tick(&up(STS), STS + 40, 0.20, false),
                    print_tick(&up(STS), STS + 100, 0.50, false),
                    print_tick(&up(STS), STS + 200, 0.50, false),
                    print_tick(&up(STS), STS + 250, 0.50, false),
                ],
            ),
            (
                dn(STS),
                vec![
                    print_tick(&dn(STS), STS + 140, 0.20, false),
                    print_tick(&dn(STS), STS + 200, 0.50, false),
                    print_tick(&dn(STS), STS + 250, 0.50, false),
                ],
            ),
        ];

        // sanity: the two prints really do straddle the gate the way the test intends
        let hold = run(CheapNp::new(SPOT), series.clone());
        assert_eq!(hold.strategy.entries, 1);
        assert_eq!(hold.strategy.flips, 0, "Hold never flips");
        assert_eq!(hold.core.position_of(&up(STS)).size, 1.0);
        assert_eq!(hold.core.position_of(&dn(STS)).size, 0.0);

        let mut s = CheapNp::new(SPOT);
        s.mode = CheapNpMode::Flip;
        let flip = run(s, series);
        assert_eq!(flip.strategy.entries, 1);
        assert_eq!(flip.strategy.flips, 1, "the reversal is a run start -> one flip");
        assert_eq!(flip.core.position_of(&up(STS)).size, 0.0, "the Up token was sold");
        assert_eq!(flip.core.position_of(&dn(STS)).size, 1.0, "and Down entered");
    }

    #[test]
    fn flip_mode_ignores_repeat_prints_on_the_held_side() {
        let prints = vec![
            print_tick(&up(STS), STS + 40, 0.20, false),
            print_tick(&up(STS), STS + 60, 0.20, false),
            print_tick(&up(STS), STS + 80, 0.20, false),
        ];
        let mut s = CheapNp::new(SPOT);
        s.mode = CheapNpMode::Flip;
        let e = run(s, with_warm_spot(prints, 0.4));
        assert_eq!((e.strategy.entries, e.strategy.flips), (1, 0), "same side = not a run start");
    }

    #[test]
    fn signals_record_the_fired_print_not_the_fill() {
        let prints = vec![
            print_tick(&up(STS), STS + 10, 0.20, false), // out of time
            print_tick(&up(STS), STS + 40, 0.20, false), // THE entry
            print_tick(&up(STS), STS + 60, 0.31, false), // later print — the fill price, not a signal
        ];
        let e = run(CheapNp::new(SPOT), with_warm_spot(prints, 0.4));
        assert_eq!(e.strategy.signals.len(), 1, "one signal per fired gate");
        let s = e.strategy.signals[0];
        assert_eq!((s.sts, s.oidx, s.is_flip), (STS, 0, false));
        assert_eq!(s.ts, (STS + 40) * 1000, "the PRINT's ts, in ms");
        assert_eq!(s.ask, 0.20, "the price the gate scored, not the next print's 0.31");
        // and it is the gate's own edge, not a recomputation
        assert!(s.edge > THETA, "{}", s.edge);
        assert_eq!(e.strategy.signals.len(), e.strategy.entries + e.strategy.flips);
    }

    #[test]
    fn flip_signals_are_tagged_and_ordered() {
        let prints = vec![
            print_tick(&up(STS), STS + 40, 0.20, false),
            print_tick(&dn(STS), STS + 140, 0.20, false),
        ];
        // reuse the explicit reversal path from the flip test above by driving Hold's spot shape:
        // here a constant up-drift means the DOWN print cannot qualify, so only the entry fires.
        let mut s = CheapNp::new(SPOT);
        s.mode = CheapNpMode::Flip;
        let e = run(s, with_warm_spot(prints, 0.4));
        assert_eq!(e.strategy.signals.len(), 1);
        assert!(!e.strategy.signals[0].is_flip, "the first fire is never a flip");
        assert_eq!(e.strategy.signals.len(), e.strategy.entries + e.strategy.flips);
    }

    #[test]
    fn from_params_reads_every_knob_and_defaults_the_rest() {
        let p: toml::Value = toml::from_str(
            r#"
spot_symbol = "BTCUSDT"
mode = "FLIP"
size = 3
theta = 0.02
h = 900.0
sigma_lookback_s = 600
sigma_scale = 1.25
taker_buys_only = false
"#,
        )
        .unwrap();
        let s = CheapNp::from_params(&p);
        assert_eq!(s.spot_symbol, "BTCUSDT");
        assert_eq!(s.mode, CheapNpMode::Flip);
        assert_eq!((s.size, s.theta, s.h, s.sigma_lookback_s), (3.0, 0.02, 900.0, 600.0));
        assert_eq!(s.sigma_scale, 1.25);
        assert!(!s.taker_buys_only);

        let d = CheapNp::from_params(&toml::Value::Table(Default::default()));
        assert_eq!(d.mode, CheapNpMode::Hold);
        assert_eq!((d.size, d.theta, d.h, d.sigma_lookback_s), (1.0, THETA, H, SIGMA_LOOKBACK_S));
        assert_eq!(d.sigma_scale, 1.0, "the oracle scale is the default and must stay 1.0");
        assert!(d.taker_buys_only);
        assert_eq!(d.spot_symbol, "");
    }

    /// The σ knob is a sensitivity dial, so what has to hold is (a) it is INERT at the default and
    /// (b) it moves the edge the way σ actually moves it.
    ///
    /// That direction is NOT "bigger σ ⇒ fewer entries", and assuming so is a trap this test exists
    /// to pin: σ only ever pulls `p_up` toward 0.5. For a side the model already makes the
    /// FAVOURITE (`prob > 0.5`) a bigger σ shrinks the edge; for a side it makes the UNDERDOG a
    /// bigger σ GROWS it — and the cheap band `[0.10, 0.35)` is full of underdogs, whose gate
    /// clears at `prob > ask + fee + θ ≈ 0.27`, well below 0.5. So a σ change reshuffles WHICH
    /// windows qualify rather than uniformly loosening or tightening the gate.
    #[test]
    fn sigma_scale_is_inert_at_one_and_pulls_the_edge_toward_the_coin_flip() {
        let prints = vec![print_tick(&up(STS), STS + 40, 0.20, false)];
        let series = with_warm_spot(prints, 0.4);

        let base = run(CheapNp::new(SPOT), series.clone());
        let mut unit = CheapNp::new(SPOT);
        unit.sigma_scale = 1.0;
        let same = run(unit, series.clone());
        assert_eq!(same.strategy.signals, base.strategy.signals, "scale 1.0 must change nothing");

        let mut wide = CheapNp::new(SPOT);
        wide.sigma_scale = 4.0;
        let wide = run(wide, series.clone());
        let mut tight = CheapNp::new(SPOT);
        tight.sigma_scale = 0.25;
        let tight = run(tight, series);

        // The spot drifts UP and the entered side is Up, so the model already makes it the
        // favourite: p_up > 0.5 and a bigger σ pulls it DOWN toward the coin flip.
        assert_eq!((base.strategy.entries, wide.strategy.entries), (1, 1));
        let (b, w, t) = (
            base.strategy.signals[0].edge,
            wide.strategy.signals[0].edge,
            tight.strategy.signals[0].edge,
        );
        assert!(w < b, "4x σ must shrink a favourite's edge: {w} vs {b}");
        // Only `>=`, deliberately: this fixture's drift already saturates `p_up` at 1.0, so the
        // edge sits at its CEILING (`1 − ask − fee`) and shrinking σ has nowhere left to push it.
        // Asserting `>` would be asserting a property of the fixture, not of the knob.
        assert!(t >= b, "0.25x σ must not shrink it: {t} vs {b}");
        assert_eq!(b, 1.0 - 0.20 - crate::fair_value::fee(0.20), "the fixture is at the ceiling");
        // ...and the 4x shrink is bounded below by the COIN-FLIP edge, never by zero: σ can only
        // ever pull `prob` to 0.5, which for a 0.20 ask still leaves a fat positive edge. That
        // bound is exactly why "bigger σ ⇒ fewer entries" is false inside the cheap band.
        assert!(w > 0.5 - 0.20 - crate::fair_value::fee(0.20), "σ can only reach p=0.5: {w}");
    }

    // -----------------------------------------------------------------------------------------
    // the RESTING-ASK gate (opt-in). See `crate::cheap_np_ask` for why the print price is wrong.
    // -----------------------------------------------------------------------------------------

    fn ask_gated() -> CheapNp {
        let mut s = CheapNp::new(SPOT);
        s.entry_price = EntryPrice::RestingAsk;
        s
    }

    /// THE fix, end to end: the print clears θ, the resting ask does not, so there is NO TRADE.
    ///
    /// The fixture's print is 0.20 against a saturated `p_up`, so `prob_wc = 1.0` and the
    /// θ-clearing limit sits at ~0.884 — deliberately generous, because the point being pinned is
    /// the MECHANISM, not one calibration. The book's only ask is above that limit.
    #[test]
    fn the_resting_ask_gate_refuses_a_signal_whose_edge_dies_on_the_book() {
        let e = run(ask_gated(), with_book(&[(0.95, 10_000.0)]));
        assert_eq!(e.strategy.entries, 0, "the ask is above the θ-clearing price: not a trade");
        assert_eq!(e.strategy.ask_rejects, 1);
        assert_eq!(e.strategy.ask_no_book, 0, "there WAS a book — this is a refusal, not a gap");
        assert!(e.strategy.signals.is_empty(), "a refused signal is not recorded as an entry");
        // ...and the very same tape enters under the frozen PRINT gate. That gap is the finding.
        let p = run(CheapNp::new(SPOT), with_book(&[(0.95, 10_000.0)]));
        assert_eq!(p.strategy.entries, 1);
    }

    /// The entry is priced at the BOOK's vwap (not the print) and sized to in-edge depth.
    #[test]
    fn the_resting_ask_gate_prices_at_the_book_and_sizes_to_in_edge_depth() {
        let mut s = ask_gated();
        s.size = 500.0;
        // 40 shares inside any sane limit, then a wall at 0.95 that no in-edge sweep can reach
        let e = run(s, with_book(&[(0.30, 25.0), (0.32, 15.0), (0.95, 10_000.0)]));
        assert_eq!(e.strategy.entries, 1);
        let sig = e.strategy.signals[0];
        assert_eq!(sig.qty, 40.0, "sized to the resting in-edge depth, not to the 500 requested");
        let want_vwap = (0.30 * 25.0 + 0.32 * 15.0) / 40.0;
        assert!((sig.ask - want_vwap).abs() < 1e-12, "{} vs {want_vwap}", sig.ask);
        assert_eq!(sig.print_px, 0.20, "the print is recorded, and it is NOT the price paid");
        assert!(sig.ask > sig.print_px, "the book is strictly worse than the tape print");
        // the recorded edge is the RE-SCORED one, and it still clears the bar
        assert_eq!(sig.edge, crate::cheap_np_ask::edge_at(1.0, sig.ask));
        assert!(sig.edge > THETA);
    }

    /// A book-less run must look like ZERO entries, never like a good backtest: the ask gate has
    /// nothing to gate on, so it refuses and says WHY (`ask_no_book`, not `ask_rejects`).
    #[test]
    fn without_a_book_the_resting_ask_gate_refuses_rather_than_falling_back_to_the_print() {
        let e = run(
            ask_gated(),
            with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4),
        );
        assert_eq!(e.strategy.entries, 0);
        assert_eq!(e.strategy.ask_no_book, 1, "a missing book is a DATA gap, counted as one");
        assert_eq!(e.strategy.ask_rejects, 0, "...and never reported as 'the edge was gone'");
    }

    /// DEFAULT-OFF, proven: a book being present changes nothing for the frozen print gate, so
    /// every published `cheap_np` number stands.
    #[test]
    fn the_print_gate_is_the_default_and_a_book_does_not_perturb_it() {
        assert_eq!(CheapNp::new(SPOT).entry_price, EntryPrice::Print);
        assert_eq!(CheapNp::default().entry_price, EntryPrice::Print);
        let bookless = run(
            CheapNp::new(SPOT),
            with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4),
        );
        let booked = run(CheapNp::new(SPOT), with_book(&[(0.30, 25.0), (0.95, 10_000.0)]));
        assert_eq!(booked.strategy.signals, bookless.strategy.signals);
        assert_eq!(booked.strategy.signals[0].ask, 0.20, "still the print's own price");
        assert_eq!(booked.strategy.signals[0].qty, 1.0, "still the requested size");
        assert_eq!((booked.strategy.ask_rejects, booked.strategy.ask_no_book), (0, 0));
    }

    /// Polymarket's venue-enforced 250 ms taker delay, modelled by composing the two ENGINE seams
    /// (`LatencyModelKind::venue_hold_ms` + `FillModelKind::L2Book`) rather than by any code in
    /// this strategy: the decision is taken on the book at `T`, the matching engine looks at
    /// `T + 250 ms`, and an order that no longer crosses is BOOKED — it rests, which is neither a
    /// fill nor a miss.
    #[test]
    fn the_venue_taker_delay_rests_an_order_the_market_ran_away_from() {
        // A book event and a trade at ms precision — the timeline below turns on sub-second
        // stamps, because 250 ms is SUB-BLOCK: what moves inside the hold is the book, not the
        // tape.
        let book_ms = |ts_ms: i64, asks: Vec<(f64, f64)>| {
            Tick::Book(vike_model::BookUpdate {
                ts: ts_ms,
                local_ts: ts_ms,
                seq: ts_ms as u64,
                kind: vike_model::BookUpdateKind::Snapshot,
                tick_size: 0.001,
                bids: vec![(0.05, 1_000.0)],
                asks,
                symbol: up(STS),
            })
        };
        // 0.50 is OUTSIDE the cheap band, so these can never re-arm the gate; they exist only to
        // give the engine a price event on which to run its fill pass.
        let filler_ms = |ts_ms: i64| {
            Tick::Trade(TradeTick {
                ts: ts_ms,
                local_ts: ts_ms,
                price: 0.50,
                size: 1.0,
                is_buyer_maker: false,
                symbol: up(STS),
            })
        };
        let t = (STS + 40) * 1000; // the print, and the decision instant

        let mut series = with_warm_spot(vec![print_tick(&up(STS), STS + 40, 0.20, false)], 0.4);
        {
            let ups = series.iter_mut().find(|(s, _)| *s == up(STS)).expect("the up stream");
            // decision book: cheap and deep, well inside the θ-clearing limit
            ups.1.insert(0, book_ms((STS + 20) * 1000, vec![(0.30, 500.0)]));
            // +100 ms — INSIDE the venue's 250 ms hold, so only a zero-hold order matches here
            ups.1.push(filler_ms(t + 100));
            // +200 ms — still inside the hold, and the market runs away
            ups.1.push(book_ms(t + 200, vec![(0.99, 10_000.0)]));
            // +300 ms — the hold has expired; this is where a held order is matched
            ups.1.push(filler_ms(t + 300));
        }
        let params = |latency| EngineParams {
            cash: 1000.0,
            fill_model: crate::engine::FillModelKind::L2Book,
            slippage: 0.0, // the walk IS the slippage
            latency_model: latency,
            ..Default::default()
        };

        // No hold: the order is matched at +100 ms, against the book it was decided on.
        let now = run_with(ask_gated(), series.clone(), params(None));
        assert_eq!(now.strategy.entries, 1, "the gate fired either way");
        assert_eq!(now.core.position_of(&up(STS)).size, 1.0, "filled at the decision book");
        assert_eq!(now.strategy.signals[0].ask, 0.30, "priced at the resting ask, not the print");

        // With the venue's 250 ms hold: the SAME decision at the SAME limit, but the matching
        // engine does not look until +250 ms — by which time the book no longer crosses. The
        // order is booked, i.e. it rests: not a fill, and not a miss.
        let held = run_with(
            ask_gated(),
            series,
            params(Some(crate::latency::LatencyModelKind::venue_hold_ms(
                crate::latency::VENUE_HOLD_POLYMARKET_UPDOWN_MS,
            ))),
        );
        assert_eq!(held.strategy.entries, 1, "the DECISION is unchanged — only the outcome moves");
        assert_eq!(held.strategy.signals, now.strategy.signals, "...byte-identically so");
        assert_eq!(
            held.core.position_of(&up(STS)).size,
            0.0,
            "booked, not filled: the market ran away inside the venue's hold"
        );
        // and it really is RESTING — still working, not silently dropped
        assert_eq!(held.core.pending_of(&up(STS)).len(), 1, "the order is on the book");
    }

    #[test]
    fn from_params_reads_the_entry_price_knob_and_defaults_to_the_frozen_print_gate() {
        let p = |s: &str| toml::from_str::<toml::Value>(s).unwrap();
        assert_eq!(
            CheapNp::from_params(&p("entry_price = \"resting_ask\"")).entry_price,
            EntryPrice::RestingAsk
        );
        assert_eq!(
            CheapNp::from_params(&p("entry_price = \"RESTING-ASK\"")).entry_price,
            EntryPrice::RestingAsk
        );
        // a typo must NOT silently change which trades a published run takes
        assert_eq!(
            CheapNp::from_params(&p("entry_price = \"restingask\"")).entry_price,
            EntryPrice::Print
        );
        assert_eq!(CheapNp::from_params(&p("size = 1")).entry_price, EntryPrice::Print);
        assert!(CheapNp::from_params(&p("size = 1")).limit_at_edge);
        assert!(!CheapNp::from_params(&p("limit_at_edge = false")).limit_at_edge);
    }

    #[test]
    fn the_gate_the_strategy_fires_on_is_the_shared_cheap_gate() {
        // Whatever the engine-driven entry test above enters on, the pure gate agrees with — this
        // is the link that makes the parity harness (which drives `cheap_gate` directly) a proof of
        // the STRATEGY, not of a second implementation.
        let (ask, s_now, s_open, sigma, t) = (0.20, 60_030.0, 60_000.0, 8.0e-5, 40.0);
        let e = wc(0, ask, s_now, s_open, sigma, t, H);
        assert_eq!(cheap_gate(0, ask, s_now, s_open, sigma, t, THETA, H), Some(e));
        assert!(e > THETA);
    }
}
