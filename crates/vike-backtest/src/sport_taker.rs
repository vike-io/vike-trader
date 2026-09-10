//! `sport_taker` — the Polymarket sports/esports COPY-TRADING taker.
//!
//! Port of the live paper bot `vike_db_data_jobs/trading/sport_taker/{strategy,signal,bot}.py`
//! (the oracle; read-only on the latency box). The workspace's SECOND native strategy after
//! [`crate::cheap_np`], and the first that is driven by ANOTHER PARTICIPANT'S tape rather than
//! by the market's own price.
//!
//! # The rule
//!
//! Three fixed wallets ([`WALLETS`]) are watched. For each `(wallet, condition_id,
//! outcome_index)` the wallet's BUY fills are accumulated as `Σ size·price`; the FIRST time that
//! cumulative notional reaches [`CONVICTION_USD`] the key fires ONE signal — one-shot, the key
//! is latched forever after (`signal.py`'s `_opened` set). The copier then buys the same outcome
//! token at a flat [`STAKE_USD`] and HOLDS TO SETTLEMENT; `shares = stake / price` and
//! `pnl = won ? shares − stake : −stake` (`bot.py`'s `_shares` + `common.paper.settle_pnl`).
//!
//! `ideal_price` — the wallet's own running VWAP at the crossing fill ([`vwap`]) — is recorded
//! but is explicitly UNREACHABLE by a copier: it averages fills that happened before the copier
//! could possibly have known about the position. It is the ceiling, not a lane.
//!
//! A 4th wallet (`0x…bca08c`, MLB) was DROPPED by the Python on 2026-06-14 because its edge did
//! not survive copy delay (15.8 % ideal → 2.4 %/2.6 %/−0.8 % at +2 s/+5 s/+10 s). It is
//! deliberately NOT in [`WALLETS`]; do not re-add it.
//!
//! # What this strategy consumes, and the symbol grammar
//!
//! There is no "which wallet printed this" field anywhere in [`TradeTick`] — and there should not
//! be, since that is a Polymarket-specific attribution the model layer has no business carrying.
//! So the wallet identity travels IN the symbol, exactly the way [`crate::cheap_np`] carries a
//! 5-minute window's identity in its slug:
//!
//! ```text
//!   0x29b5…cc6c@0xabcd…#1     SIGNAL series — one copied wallet's own BUY fills on one token
//!   0xabcd…#1                 MARKET series — the public taker-BUY tape of that same token
//!   ^^^^^^^^ ^                <condition_id>#<outcome_index>
//! ```
//!
//! The strategy folds SIGNAL ticks into its [`SignalTracker`] and never trades them; it submits
//! its market BUY against the MARKET symbol, which is also the symbol the engine's
//! [`crate::EngineParams::resolution`] source settles at the binary payout. A tick whose symbol
//! parses as neither is ignored (never traded) — the same "unknown symbol is inert" property
//! `cheap_np` relies on to be handed a spot reference series safely.
//!
//! `TradeTick::is_buyer_maker` carries the copied wallet's ROLE on a SIGNAL tick (`true` = the
//! wallet was the resting maker). [`SportTaker::include_maker_fills`] decides whether those count
//! toward conviction — they are ~23 % of the three wallets' BUY fills, so the choice is material
//! and must be explicit rather than implied by whatever the exporter happened to select.
//!
//! # Sizing: why the engine trades ONE unit
//!
//! The Python's stake is flat in DOLLARS, so its share count depends on the price it fills at —
//! which is not known at submit time. Rather than guess a quantity, the strategy submits
//! [`SportTaker::qty`] units (1.0 by default = one share) and the flat-stake economics are
//! recovered ANALYTICALLY from the booked `(entry_price, payout)` pair by [`flat_stake_pnl`],
//! which is algebraically identical to `settle_pnl(won, stake/price, stake)`. This is the same
//! separation `cheap_np_run` uses for the fee curve the engine seam cannot express: the engine
//! models the FILL, the reporter models the STAKE.
//!
//! # Copy latency is the engine's job, not a haircut
//!
//! The measured cost of copying these wallets lands almost entirely between the wallet's fill and
//! the copier's first look at the market (the live bot's own +2 s/+5 s snapshots move the price by
//! at most 0.0018 and are IDENTICAL to the live snapshot 99.6 % of the time on the CS2 wallet).
//! So the axis with real content is DETECTION delay δ, and it is modelled the honest way:
//! [`crate::EngineParams::latency_model`] delays the submitted order's visibility by δ, and the
//! engine fills it at the next print of the token's own MARKET stream at `ts >= signal_ts + δ` —
//! or NOT AT ALL if the market resolves first. A next-print haircut in SQL cannot express that
//! miss.

use std::collections::{HashMap, HashSet};

use vike_model::{Broker, Strategy, TradeTick};

/// The three fixed copy targets (`strategy.py:WALLETS`). Tennis was intentionally excluded
/// (too choppy in-sample) and the MLB wallet dropped — see the module doc.
pub const WALLETS: [&str; 3] = [
    "0x32ed517a571c01b6e9adecf61ba81ca48ff2f960",
    "0x29b52d98ac9ef9414b04164246c95bc63d74cc6c",
    "0x31864feb9d25dee93728c6225ba891530967e9ca",
];

/// `strategy.py:STAKE_USD` — flat dollars per copied bet.
pub const STAKE_USD: f64 = 100.0;

/// `strategy.py:CONVICTION_USD` — cumulative BUY notional a `(wallet, cid, oi)` must reach.
pub const CONVICTION_USD: f64 = 2000.0;

/// `strategy.py:SEGMENTS` — the wallet's market segment, carried through to the signal row.
/// An unknown wallet maps to `""`, exactly like the Python's `SEGMENTS.get(w, "")`.
pub fn segment_for(wallet: &str) -> &'static str {
    match wallet {
        "0x32ed517a571c01b6e9adecf61ba81ca48ff2f960" => "multi",
        "0x29b52d98ac9ef9414b04164246c95bc63d74cc6c"
        | "0x31864feb9d25dee93728c6225ba891530967e9ca" => "esports",
        _ => "",
    }
}

/// `strategy.py:vwap` — volume-weighted average buy price; `0.0` when no shares (div-by-zero
/// guard, NOT a price).
pub fn vwap(buy_usd: f64, buy_size: f64) -> f64 {
    if buy_size > 0.0 { buy_usd / buy_size } else { 0.0 }
}

/// `strategy.py:crossed` — true once cumulative buy REACHES the threshold (`>=`, not `>`).
pub fn crossed(cum_buy_usd: f64, threshold: f64) -> bool {
    cum_buy_usd >= threshold
}

/// Flat-$`stake` settlement PnL of one copied bet filled at `price` and paid `payout` ∈ {0, 1}.
///
/// Algebraically `settle_pnl(won, stake/price, stake)` = `shares − stake` on a win, `−stake` on a
/// loss — written as one expression so the reporter never has to materialize a share count.
/// A non-positive `price` yields `0.0` (the Python's `_shares` guard: no shares, no bet).
pub fn flat_stake_pnl(price: f64, payout: f64, stake: f64) -> f64 {
    if price <= 0.0 {
        return 0.0;
    }
    payout * (stake / price) - stake
}

/// One BUY fill handed to the [`SignalTracker`] — the Rust twin of the data-API trade dict
/// `signal.py:ingest` folds. Borrowed, not owned: the tracker copies only what it latches.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WalletBuy<'a> {
    /// `proxyWallet`.
    pub wallet: &'a str,
    /// `conditionId`.
    pub condition_id: &'a str,
    /// `outcomeIndex`.
    pub outcome_index: u8,
    /// `asset` — the ERC-1155 token id. Carried through to the signal for the CLOB snapshot the
    /// live bot takes; the backtest uses it only as an identifier.
    pub asset: &'a str,
    /// `slug`.
    pub slug: &'a str,
    /// Shares bought in this fill.
    pub size: f64,
    /// Price paid in this fill.
    pub price: f64,
    /// `transactionHash` — the primary component of the dedup key.
    pub tx_hash: &'a str,
}

/// A fired conviction crossing — the Rust twin of the dict `signal.py:ingest` appends to `out`.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub copied_wallet: String,
    pub segment: &'static str,
    pub condition_id: String,
    pub outcome_index: u8,
    pub asset: String,
    pub slug: String,
    /// Cumulative BUY notional at the crossing fill, rounded to 4 dp (`round(_usd[key], 4)`).
    pub trigger_buy_usd: f64,
    /// The wallet's own VWAP at the crossing fill, rounded to 6 dp (`round(vwap(..), 6)`).
    /// UNREACHABLE by a copier — see the module doc.
    pub ideal_price: f64,
}

/// The dedup key `signal.py:_trade_key` builds when a `transactionHash` is present:
/// `(tx, asset, side.upper(), size, price)`. Side is always `"BUY"` here (the tracker skips
/// everything else before keying), so it is not stored.
///
/// The float components are keyed by their IEEE-754 bit pattern — the exact-equality semantics
/// Python's own tuple hashing gives them, without needing `Eq` on `f64`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct TradeKey {
    tx_hash: String,
    asset: String,
    size_bits: u64,
    price_bits: u64,
}

/// `(wallet, condition_id, outcome_index)` — the accumulation/one-shot key.
type PosKey = (String, String, u8);

/// PURE in-memory conviction detector — the port of `signal.py:SignalTracker`.
///
/// Accumulates each watched wallet's BUY notional per `(wallet, cid, outcome_index)` and emits a
/// ONE-SHOT signal the first time a key reaches the threshold. No I/O.
///
/// # Two intake shapes, and why both exist
///
/// * [`SignalTracker::ingest`] is the faithful BATCH port, `_seen`-prune included: the live bot
///   re-reads the same ~200-row data-API window every 5 s, so without per-trade dedup one fill
///   would be counted ~74× a minute.
/// * [`SignalTracker::ingest_one`] is the TAPE shape this crate's backtest uses. An on-chain fill
///   appears exactly once in the tape, so there is nothing to dedup — but the Python's dedup key
///   is LOSSY (it does not include `log_index`), so two distinct fills in one transaction with
///   identical size AND price collapse into one. `ingest_one` therefore applies the same key and
///   counts each collapse in [`SignalTracker::dedup_collisions`] rather than silently dropping or
///   silently keeping it. That count is a measurement of the live bot's own accumulation error.
#[derive(Debug, Clone)]
pub struct SignalTracker {
    threshold: f64,
    usd: HashMap<PosKey, f64>,
    size: HashMap<PosKey, f64>,
    opened: HashSet<PosKey>,
    seen: HashSet<TradeKey>,
    dedup_collisions: u64,
}

impl SignalTracker {
    /// A tracker armed at `threshold` dollars of cumulative BUY notional.
    pub fn new(threshold: f64) -> Self {
        SignalTracker {
            threshold,
            usd: HashMap::new(),
            size: HashMap::new(),
            opened: HashSet::new(),
            seen: HashSet::new(),
            dedup_collisions: 0,
        }
    }

    /// `signal.py:seed_open` — adopt keys that already have a bet (a restart must never re-open
    /// them).
    pub fn seed_open<I, W, C>(&mut self, keys: I)
    where
        I: IntoIterator<Item = (W, C, u8)>,
        W: Into<String>,
        C: Into<String>,
    {
        for (w, c, oi) in keys {
            self.opened.insert((w.into(), c.into(), oi));
        }
    }

    /// The live threshold (hot-reloaded from `trade_bot_config` in the Python).
    pub fn threshold(&self) -> f64 {
        self.threshold
    }

    /// Overwrite the threshold — the Python's `self.tracker.threshold = float(conv)` hot-reload.
    pub fn set_threshold(&mut self, threshold: f64) {
        self.threshold = threshold;
    }

    /// How many tape fills the Python's `(tx, asset, side, size, price)` dedup key collapsed —
    /// see the type doc. Always `0` for [`SignalTracker::ingest`] over a real data-API window.
    pub fn dedup_collisions(&self) -> u64 {
        self.dedup_collisions
    }

    /// Number of latched (already fired or seeded) keys — `len(self._opened)`.
    pub fn opened_len(&self) -> usize {
        self.opened.len()
    }

    /// Cumulative BUY notional currently accumulated for a key (`0.0` if untracked). Exists for
    /// tests and for the runner's diagnostics; the strategy never reads it.
    pub fn cum_usd(&self, wallet: &str, condition_id: &str, outcome_index: u8) -> f64 {
        self.usd
            .get(&(wallet.to_string(), condition_id.to_string(), outcome_index))
            .copied()
            .unwrap_or(0.0)
    }

    /// Fold ONE fill (the tape shape). Returns the signal if this fill crossed the threshold.
    ///
    /// `emit = false` mirrors `ingest(..., emit=False)`: the key is still latched (adopted) but no
    /// signal is produced — the live bot's startup prime pass.
    pub fn ingest_one(&mut self, b: &WalletBuy<'_>, emit: bool) -> Option<Signal> {
        let key = (b.wallet.to_string(), b.condition_id.to_string(), b.outcome_index);
        if self.opened.contains(&key) {
            return None;
        }
        let tkey = TradeKey {
            tx_hash: b.tx_hash.to_string(),
            asset: b.asset.to_string(),
            size_bits: b.size.to_bits(),
            price_bits: b.price.to_bits(),
        };
        if !self.seen.insert(tkey) {
            self.dedup_collisions += 1;
            return None;
        }
        let usd = self.usd.entry(key.clone()).or_insert(0.0);
        *usd += b.size * b.price;
        let cum_usd = *usd;
        let size = self.size.entry(key.clone()).or_insert(0.0);
        *size += b.size;
        let cum_size = *size;
        if !crossed(cum_usd, self.threshold) {
            return None;
        }
        self.opened.insert(key);
        if !emit {
            return None;
        }
        Some(Signal {
            copied_wallet: b.wallet.to_string(),
            segment: segment_for(b.wallet),
            condition_id: b.condition_id.to_string(),
            outcome_index: b.outcome_index,
            asset: b.asset.to_string(),
            slug: b.slug.to_string(),
            trigger_buy_usd: round_dp(cum_usd, 4),
            ideal_price: round_dp(vwap(cum_usd, cum_size), 6),
        })
    }

    /// Fold a data-API BATCH — the faithful `signal.py:ingest` port, `_seen` prune included.
    ///
    /// The prune (`_seen &= current_batch_keys`) bounds the dedup set to the API window: a fill
    /// that has scrolled out can never reappear, so its key is no longer needed. Cumulative
    /// `usd`/`size` for keys that have NOT yet fired are never pruned.
    pub fn ingest(&mut self, batch: &[WalletBuy<'_>], emit: bool) -> Vec<Signal> {
        let mut out = Vec::new();
        let mut current: HashSet<TradeKey> = HashSet::with_capacity(batch.len());
        for b in batch {
            let key = (b.wallet.to_string(), b.condition_id.to_string(), b.outcome_index);
            if self.opened.contains(&key) {
                continue;
            }
            current.insert(TradeKey {
                tx_hash: b.tx_hash.to_string(),
                asset: b.asset.to_string(),
                size_bits: b.size.to_bits(),
                price_bits: b.price.to_bits(),
            });
            if let Some(sig) = self.ingest_one(b, emit) {
                out.push(sig);
            }
        }
        self.seen.retain(|k| current.contains(k));
        out
    }
}

/// Round half-away-from-zero to `dp` decimal places — the reporting rounding the Python applies
/// to `trigger_buy_usd` (4 dp) and `ideal_price` (6 dp).
///
/// NOT bit-identical to CPython's `round()`, which is round-half-to-EVEN on the decimal
/// representation. The divergence needs an exact decimal tie in the 5th/7th place of a
/// `Σ size·price` accumulation, which no row of the six-month tape exhibits; the numbers this
/// produces are reporting fields, never inputs to a later decision.
fn round_dp(x: f64, dp: u32) -> f64 {
    let f = 10f64.powi(dp as i32);
    (x * f).round() / f
}

/// A SIGNAL series symbol split into its wallet and the MARKET symbol it trades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignalSymbol {
    /// The copied wallet (`0x…`).
    pub wallet: String,
    /// `<condition_id>#<outcome_index>` — the tradable token, and what the resolution source keys.
    pub market: String,
    /// `<condition_id>`.
    pub condition_id: String,
    /// `<outcome_index>`.
    pub outcome_index: u8,
}

/// Parse `"<wallet>@<condition_id>#<outcome_index>"`. `None` for anything else — including a bare
/// market symbol, which is exactly what makes the market stream inert to the signal path.
pub fn parse_signal_symbol(symbol: &str) -> Option<SignalSymbol> {
    let (wallet, market) = symbol.split_once('@')?;
    if wallet.is_empty() {
        return None;
    }
    let (condition_id, oi) = market.rsplit_once('#')?;
    if condition_id.is_empty() {
        return None;
    }
    let outcome_index: u8 = oi.parse().ok()?;
    Some(SignalSymbol {
        wallet: wallet.to_string(),
        market: market.to_string(),
        condition_id: condition_id.to_string(),
        outcome_index,
    })
}

/// Build the SIGNAL series symbol for one wallet on one token.
pub fn signal_symbol(wallet: &str, condition_id: &str, outcome_index: u8) -> String {
    format!("{wallet}@{condition_id}#{outcome_index}")
}

/// Build the MARKET (tradable) series symbol for one token.
pub fn market_symbol(condition_id: &str, outcome_index: u8) -> String {
    format!("{condition_id}#{outcome_index}")
}

/// One fired entry, recorded for the runner's report (the strategy's own audit trail — the twin
/// of [`crate::cheap_np::CheapNpSignal`]).
#[derive(Debug, Clone, PartialEq)]
pub struct FiredEntry {
    /// The crossing fill's timestamp, epoch ms — the live bot's `ts_signal`.
    pub ts: i64,
    /// The market symbol the BUY was submitted against.
    pub market: String,
    /// The crossing itself.
    pub signal: Signal,
}

/// The copy-trading strategy. Portable (`impl<B: Broker>`), so the same code runs live.
#[derive(Debug, Clone)]
pub struct SportTaker {
    /// Wallets to copy. EMPTY = copy every wallet the signal stream carries (the runner
    /// pre-filters by construction); a non-empty list is an allow-list.
    pub wallets: Vec<String>,
    /// Flat dollars per bet — reporting only (the engine trades [`SportTaker::qty`] units; see
    /// the module doc's sizing section).
    pub stake_usd: f64,
    /// Units submitted per entry.
    pub qty: f64,
    /// Count the wallet's MAKER BUY fills toward conviction. `true` mirrors the live bot, whose
    /// data-API feed does not distinguish role.
    pub include_maker_fills: bool,
    /// Every crossing that fired, in order.
    pub fired: Vec<FiredEntry>,
    tracker: SignalTracker,
}

impl Default for SportTaker {
    fn default() -> Self {
        SportTaker::new(CONVICTION_USD)
    }
}

impl SportTaker {
    /// A strategy armed at `conviction_usd`, copying [`WALLETS`], flat [`STAKE_USD`], 1 unit.
    pub fn new(conviction_usd: f64) -> Self {
        SportTaker {
            wallets: WALLETS.iter().map(|w| (*w).to_string()).collect(),
            stake_usd: STAKE_USD,
            qty: 1.0,
            include_maker_fills: true,
            fired: Vec::new(),
            tracker: SignalTracker::new(conviction_usd),
        }
    }

    /// Read the harness registry's TOML params table (`wallets`, `stake_usd`, `conviction_usd`,
    /// `qty`, `include_maker_fills`). Unrecognized keys are ignored — a params READER, like
    /// `BuyHold::from_params`.
    pub fn from_params(params: &toml::Value) -> Self {
        let mut s = SportTaker::new(
            params.get("conviction_usd").and_then(as_f64).unwrap_or(CONVICTION_USD),
        );
        if let Some(list) = params.get("wallets").and_then(toml::Value::as_array) {
            let ws: Vec<String> =
                list.iter().filter_map(toml::Value::as_str).map(str::to_string).collect();
            if !ws.is_empty() {
                s.wallets = ws;
            }
        }
        if let Some(v) = params.get("stake_usd").and_then(as_f64) {
            s.stake_usd = v;
        }
        if let Some(v) = params.get("qty").and_then(as_f64) {
            s.qty = v;
        }
        if let Some(v) = params.get("include_maker_fills").and_then(toml::Value::as_bool) {
            s.include_maker_fills = v;
        }
        s
    }

    /// Read-only view of the detector (the runner reports its dedup-collision count).
    pub fn tracker(&self) -> &SignalTracker {
        &self.tracker
    }

    /// Latch keys that already hold a bet — `signal.py:seed_open`, for a live restart.
    pub fn seed_open<I, W, C>(&mut self, keys: I)
    where
        I: IntoIterator<Item = (W, C, u8)>,
        W: Into<String>,
        C: Into<String>,
    {
        self.tracker.seed_open(keys);
    }

    fn watches(&self, wallet: &str) -> bool {
        self.wallets.is_empty() || self.wallets.iter().any(|w| w == wallet)
    }
}

impl<B: Broker> Strategy<B> for SportTaker {
    fn on_trade_tick(&mut self, broker: &mut B, t: &TradeTick) {
        // MARKET-stream prints carry no signal — they are the fill/mark surface the engine uses.
        let Some(sig_sym) = parse_signal_symbol(&t.symbol) else { return };
        if !self.watches(&sig_sym.wallet) {
            return;
        }
        // `is_buyer_maker` on a SIGNAL tick means the copied wallet was the RESTING side.
        if t.is_buyer_maker && !self.include_maker_fills {
            return;
        }
        // A `TradeTick` carries no transaction hash, so the dedup key's `tx` slot takes the
        // fill's timestamp: `(ts, token, size, price)` identifies an on-chain fill in every case
        // except the very collision the Python's own key already collapses — which the tracker
        // COUNTS rather than silently drops.
        let ts_key = t.ts.to_string();
        let buy = WalletBuy {
            wallet: &sig_sym.wallet,
            condition_id: &sig_sym.condition_id,
            outcome_index: sig_sym.outcome_index,
            asset: &sig_sym.market,
            slug: "",
            size: t.size,
            price: t.price,
            tx_hash: &ts_key,
        };
        if let Some(signal) = self.tracker.ingest_one(&buy, true) {
            self.fired.push(FiredEntry {
                ts: t.ts,
                market: sig_sym.market.clone(),
                signal: signal.clone(),
            });
            broker.submit_market(&sig_sym.market, 1, self.qty);
        }
    }
}

fn as_f64(v: &toml::Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buy<'a>(
        wallet: &'a str,
        cid: &'a str,
        oi: u8,
        size: f64,
        price: f64,
        tx: &'a str,
    ) -> WalletBuy<'a> {
        WalletBuy {
            wallet,
            condition_id: cid,
            outcome_index: oi,
            asset: "tok",
            slug: "slug",
            size,
            price,
            tx_hash: tx,
        }
    }

    const W: &str = "0x29b52d98ac9ef9414b04164246c95bc63d74cc6c";

    #[test]
    fn vwap_guards_zero_size() {
        assert_eq!(vwap(100.0, 0.0), 0.0);
        assert_eq!(vwap(100.0, 200.0), 0.5);
    }

    #[test]
    fn crossed_is_inclusive_at_the_threshold() {
        // `>=`, not `>` — an exactly-$2000 accumulation FIRES.
        assert!(crossed(2000.0, 2000.0));
        assert!(!crossed(1_999.999_9, 2000.0));
    }

    #[test]
    fn segments_match_the_python_table() {
        assert_eq!(segment_for("0x32ed517a571c01b6e9adecf61ba81ca48ff2f960"), "multi");
        assert_eq!(segment_for(W), "esports");
        assert_eq!(segment_for("0x31864feb9d25dee93728c6225ba891530967e9ca"), "esports");
        assert_eq!(segment_for("0xdeadbeef"), "");
    }

    #[test]
    fn flat_stake_pnl_equals_shares_minus_stake() {
        // Won at 0.40: shares = 100/0.40 = 250 -> +150.
        assert!((flat_stake_pnl(0.4, 1.0, 100.0) - 150.0).abs() < 1e-12);
        // Lost: exactly -stake, at any price.
        assert_eq!(flat_stake_pnl(0.4, 0.0, 100.0), -100.0);
        assert_eq!(flat_stake_pnl(0.97, 0.0, 100.0), -100.0);
        // No price -> no bet (the `_shares` guard), NOT an infinite share count.
        assert_eq!(flat_stake_pnl(0.0, 1.0, 100.0), 0.0);
    }

    #[test]
    fn tracker_fires_once_at_the_crossing_fill_with_the_wallet_vwap() {
        let mut t = SignalTracker::new(2000.0);
        // 1000 @ 0.50 = $500, then 2000 @ 0.60 = $1200 (cum $1700, below), then 1000 @ 0.55 = $550
        // -> cum $2250 CROSSES. VWAP = 2250 / 4000 = 0.5625.
        assert!(t.ingest_one(&buy(W, "c", 1, 1000.0, 0.50, "a"), true).is_none());
        assert!(t.ingest_one(&buy(W, "c", 1, 2000.0, 0.60, "b"), true).is_none());
        let s = t.ingest_one(&buy(W, "c", 1, 1000.0, 0.55, "c"), true).expect("crossed");
        assert_eq!(s.trigger_buy_usd, 2250.0);
        assert_eq!(s.ideal_price, 0.5625);
        assert_eq!(s.copied_wallet, W);
        assert_eq!(s.segment, "esports");
        assert_eq!(s.outcome_index, 1);
        // ONE-SHOT: further buys on the same key never fire again.
        assert!(t.ingest_one(&buy(W, "c", 1, 9999.0, 0.90, "d"), true).is_none());
        assert_eq!(t.opened_len(), 1);
    }

    #[test]
    fn outcome_index_and_condition_id_are_separate_keys() {
        let mut t = SignalTracker::new(2000.0);
        // $1500 on oi=0 and $1500 on oi=1 of the SAME market cross NEITHER — the key is per side.
        assert!(t.ingest_one(&buy(W, "c", 0, 3000.0, 0.5, "a"), true).is_none());
        assert!(t.ingest_one(&buy(W, "c", 1, 3000.0, 0.5, "b"), true).is_none());
        assert_eq!(t.opened_len(), 0);
        // ...and the same for a different condition_id.
        assert!(t.ingest_one(&buy(W, "d", 0, 3000.0, 0.5, "e"), true).is_none());
        assert_eq!(t.opened_len(), 0);
    }

    #[test]
    fn seed_open_suppresses_a_key_that_would_otherwise_fire() {
        let mut t = SignalTracker::new(2000.0);
        t.seed_open([(W, "c", 1u8)]);
        assert!(t.ingest_one(&buy(W, "c", 1, 10_000.0, 0.9, "a"), true).is_none());
        // ...and does not accumulate it either (the Python `continue`s before the fold).
        assert_eq!(t.cum_usd(W, "c", 1), 0.0);
    }

    #[test]
    fn emit_false_adopts_without_signalling() {
        let mut t = SignalTracker::new(2000.0);
        assert!(t.ingest_one(&buy(W, "c", 1, 5000.0, 0.5, "a"), false).is_none());
        assert_eq!(t.opened_len(), 1);
    }

    #[test]
    fn batch_ingest_dedups_a_repeated_window_row() {
        // The live failure this guards: the data-API returns the same ~200 rows every 5 s.
        let mut t = SignalTracker::new(2000.0);
        let row = buy(W, "c", 1, 1500.0, 0.5, "tx1"); // $750
        for _ in 0..10 {
            assert!(t.ingest(&[row], true).is_empty(), "a repeated row must never accumulate");
        }
        assert_eq!(t.cum_usd(W, "c", 1), 750.0);
        // A genuinely new fill in the same window then crosses.
        let sigs = t.ingest(&[row, buy(W, "c", 1, 2500.0, 0.5, "tx2")], true);
        assert_eq!(sigs.len(), 1);
        assert_eq!(sigs[0].trigger_buy_usd, 2000.0);
    }

    #[test]
    fn batch_prune_keeps_seen_bounded_to_the_window() {
        let mut t = SignalTracker::new(1e12); // never fires — we are testing the prune only
        t.ingest(&[buy(W, "c", 1, 1.0, 0.5, "old")], true);
        t.ingest(&[buy(W, "c", 1, 1.0, 0.5, "new")], true);
        // "old" scrolled out of the window, so its dedup key was dropped — a fill re-presented
        // after scrolling out would be DOUBLE-COUNTED. That is the Python's own documented
        // trade-off, pinned here so a change to it is visible.
        let before = t.cum_usd(W, "c", 1);
        t.ingest(&[buy(W, "c", 1, 1.0, 0.5, "old")], true);
        assert!(t.cum_usd(W, "c", 1) > before);
    }

    #[test]
    fn lossy_dedup_key_collapses_two_identical_fills_in_one_transaction() {
        // The Python key omits `log_index`, so two on-chain fills of the same size and price in
        // ONE transaction are indistinguishable. Counted, never silently swallowed.
        let mut t = SignalTracker::new(2000.0);
        assert!(t.ingest_one(&buy(W, "c", 1, 1000.0, 0.5, "tx"), true).is_none());
        assert!(t.ingest_one(&buy(W, "c", 1, 1000.0, 0.5, "tx"), true).is_none());
        assert_eq!(t.cum_usd(W, "c", 1), 500.0, "the second fill was dropped by the key");
        assert_eq!(t.dedup_collisions(), 1);
    }

    #[test]
    fn symbol_grammar_round_trips_and_rejects_a_market_symbol() {
        let s = signal_symbol(W, "0xabc", 1);
        assert_eq!(s, format!("{W}@0xabc#1"));
        let p = parse_signal_symbol(&s).expect("parses");
        assert_eq!(p.wallet, W);
        assert_eq!(p.market, "0xabc#1");
        assert_eq!(p.condition_id, "0xabc");
        assert_eq!(p.outcome_index, 1);
        // A bare MARKET symbol must NOT parse — that is what keeps the fill tape inert.
        assert!(parse_signal_symbol(&market_symbol("0xabc", 1)).is_none());
        assert!(parse_signal_symbol("0xabc").is_none());
        assert!(parse_signal_symbol("@0xabc#1").is_none());
        assert!(parse_signal_symbol(&format!("{W}@0xabc#x")).is_none());
    }

    #[test]
    fn from_params_reads_every_knob() {
        let toml_src = r#"
wallets = ["0xaaa", "0xbbb"]
stake_usd = 250.0
conviction_usd = 5000
qty = 2
include_maker_fills = false
"#;
        let params: toml::Value = toml::from_str(toml_src).unwrap();
        let s = SportTaker::from_params(&params);
        assert_eq!(s.wallets, vec!["0xaaa".to_string(), "0xbbb".to_string()]);
        assert_eq!(s.stake_usd, 250.0);
        assert_eq!(s.tracker.threshold(), 5000.0);
        assert_eq!(s.qty, 2.0);
        assert!(!s.include_maker_fills);
    }

    #[test]
    fn from_params_defaults_to_the_python_constants() {
        let s = SportTaker::from_params(&toml::Value::Table(Default::default()));
        assert_eq!(s.stake_usd, STAKE_USD);
        assert_eq!(s.tracker.threshold(), CONVICTION_USD);
        assert_eq!(s.qty, 1.0);
        assert!(s.include_maker_fills);
        assert_eq!(s.wallets.len(), 3);
    }
}
