//! `sport_taker` — the Polymarket sports/esports COPY-TRADING taker.
//!
//! Port of the live paper bot `vike_db_data_jobs/trading/sport_taker/{strategy,signal,bot}.py`
//! (the oracle; read-only on the latency box). Driven by ANOTHER PARTICIPANT'S tape, not the market's price.
//!
//! ```toml
//! [strategy]
//! runs_on = "backtest"
//!
//! [params.conviction_usd]
//! type = "number"
//! default = 2000.0
//!
//! [params.wallets]
//! type = "array"
//! default = [
//!     "0x32ed517a571c01b6e9adecf61ba81ca48ff2f960",
//!     "0x29b52d98ac9ef9414b04164246c95bc63d74cc6c",
//!     "0x31864feb9d25dee93728c6225ba891530967e9ca",
//! ]
//!
//! [params.stake_usd]
//! type = "number"
//! default = 100.0
//!
//! [params.qty]
//! type = "number"
//! default = 1.0
//!
//! [params.include_maker_fills]
//! type = "bool"
//! default = true
//! ```
//!
//! ⚠ A tape strategy: it overrides `on_trade_tick` ALONE, which the param-gate harness never
//! drives (`docs/decisions/0075`).
//!
//! # The rule
//!
//! Three fixed wallets ([`WALLETS`]) are watched. Per `(wallet, condition_id, outcome_index)` the
//! wallet's BUY fills accumulate `Σ size·price`; the FIRST time that reaches [`CONVICTION_USD`]
//! the key fires ONE signal and is latched forever (`signal.py`'s `_opened`). The copier buys the
//! same outcome token at a flat [`STAKE_USD`] and HOLDS TO SETTLEMENT; `shares = stake / price`,
//! `pnl = won ? shares − stake : −stake` (`bot.py`'s `_shares` + `common.paper.settle_pnl`).
//!
//! `ideal_price` — the wallet's own VWAP at the crossing fill (`vwap` in
//! `vike_strategy::strategies::sport_taker`) — is recorded but UNREACHABLE by a copier (it
//! averages fills before the copier could know): a ceiling, not a lane.
//!
//! A 4th wallet (`0x…bca08c`, MLB) was DROPPED: its edge did not survive copy delay (15.8 % ideal
//! → 2.4 %/2.6 %/−0.8 % at +2 s/+5 s/+10 s). Do not re-add it to [`WALLETS`].
//!
//! # The symbol grammar
//!
//! [`TickTrade`] has no "which wallet printed this" field (a Polymarket attribution the model
//! layer must not carry), so the wallet travels IN the symbol, as `cheap_np`'s window does:
//!
//! ```text
//!   0x29b5…cc6c@0xabcd…#1     SIGNAL series — one copied wallet's own BUY fills on one token
//!   0xabcd…#1                 MARKET series — the public taker-BUY tape of that same token
//!   ^^^^^^^^ ^                <condition_id>#<outcome_index>
//! ```
//!
//! SIGNAL ticks fold into the [`SignalTracker`] and are never traded; the market BUY goes to the
//! MARKET symbol, which `vike_simulator::StrategyEngineConfig::resolution` settles at the binary payout. A tick
//! parsing as neither is inert (never traded).
//!
//! On a SIGNAL tick `TickTrade::is_buyer_maker` is the copied wallet's ROLE (`true` = resting
//! maker). [`SportTaker::include_maker_fills`] decides whether those count toward conviction: ~23 %
//! of the wallets' BUY fills, so the choice is material and explicit.
//!
//! # Sizing: the engine trades ONE unit
//!
//! The flat DOLLAR stake's share count depends on an unknown fill price, so the strategy submits
//! [`SportTaker::qty`] units (default one share) and [`vike_strategy::flat_stake_pnl`] recovers the flat-stake
//! economics ANALYTICALLY from the booked `(entry_price, payout)`: the engine models the FILL, the
//! reporter models the STAKE (as `cheap_np_run` does for its fee curve).
//!
//! # Copy latency is the engine's job, not a haircut
//!
//! The cost of copying lands almost entirely in DETECTION delay δ (the live bot's +2 s/+5 s
//! snapshots move the price by at most 0.0018). `vike_simulator::StrategyEngineConfig::latency_model` delays the
//! order by δ and the engine fills it at the next MARKET print at `ts >= signal_ts + δ` — or NOT AT
//! ALL if the market resolves first, a miss a next-print SQL haircut cannot express.

use vike_marketdata::TickTrade;
use vike_model::params::{ParamValue, Params};
use vike_model::{OrderBroker, OrderPlacement, Strategy};
use vike_strategy::strategies::sport_taker::parse_signal_symbol;
use vike_strategy::{SignalCopyTrade, SignalTracker, WalletBuy};

/// The strategy this file runs, built from the profile's `[strategy.params]`.
pub fn build<B: OrderBroker + 'static>(params: &Params) -> Box<dyn Strategy<B> + Send> {
    Box::new(SportTaker::from_params(params))
}

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

/// One fired entry, recorded for the runner's report (the strategy's own audit trail — the twin
/// of the up/down fair-value taker's `CheapNpSignal`).
#[derive(Debug, Clone, PartialEq)]
pub struct FiredEntry {
    /// The crossing fill's timestamp, epoch ms — the live bot's `timestamp_signal`.
    pub timestamp: i64,
    /// The market symbol the BUY was submitted against.
    pub market: String,
    /// The crossing itself.
    pub signal: SignalCopyTrade,
}

/// The copy-trading strategy. Portable (`impl<B: OrderBroker>`), so the same code runs live.
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

    /// Read the harness registry's params table (`wallets`, `stake_usd`, `conviction_usd`,
    /// `qty`, `include_maker_fills`). Unrecognized keys are ignored — a params READER, like
    /// `BuyHold::from_params`.
    pub fn from_params(params: &Params) -> Self {
        let mut s = SportTaker::new(
            params.get("conviction_usd").and_then(ParamValue::as_f64).unwrap_or(CONVICTION_USD),
        );
        if let Some(list) = params.get("wallets").and_then(ParamValue::as_array) {
            let ws: Vec<String> =
                list.iter().filter_map(ParamValue::as_str).map(str::to_string).collect();
            if !ws.is_empty() {
                s.wallets = ws;
            }
        }
        if let Some(v) = params.get("stake_usd").and_then(ParamValue::as_f64) {
            s.stake_usd = v;
        }
        if let Some(v) = params.get("qty").and_then(ParamValue::as_f64) {
            s.qty = v;
        }
        if let Some(v) = params.get("include_maker_fills").and_then(ParamValue::as_bool) {
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

impl<B: OrderBroker> Strategy<B> for SportTaker {
    fn on_trade_tick(&mut self, broker: &mut B, t: &TickTrade) {
        // MARKET-stream prints carry no signal — they are the fill/mark surface the engine uses.
        let Some(sig_symbol) = parse_signal_symbol(&t.symbol) else { return };
        if !self.watches(&sig_symbol.wallet) {
            return;
        }
        // `is_buyer_maker` on a SIGNAL tick means the copied wallet was the RESTING side.
        if t.is_buyer_maker && !self.include_maker_fills {
            return;
        }
        // No transaction hash on a `TickTrade`: the key's `tx` slot takes the timestamp, which
        // identifies a fill except in the collision the Python's key already collapses (COUNTED).
        let timestamp_key = t.timestamp.to_string();
        let buy = WalletBuy {
            wallet: &sig_symbol.wallet,
            condition_id: &sig_symbol.condition_id,
            outcome_index: sig_symbol.outcome_index,
            asset: &sig_symbol.market,
            slug: "",
            size: t.size,
            price: t.price,
            tx_hash: &timestamp_key,
        };
        if let Some(signal) = self.tracker.ingest_one(&buy, true) {
            self.fired.push(FiredEntry {
                timestamp: t.timestamp,
                market: sig_symbol.market.clone(),
                signal: signal.clone(),
            });
            broker.place(&sig_symbol.market, "", 1, self.qty, OrderPlacement::Market, false);
        }
    }
}

// Copied from `crates/vike-strategy/src/strategies/sport_taker_tests.rs` (the strategy half's two;
// the tracker's own stay with the tracker). Run by `crates/vike-strategies-check`
// (`cargo test -p vike-strategies-check`); a build of this file compiles no test.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_params_reads_every_knob() {
        let toml_source = r#"
    wallets = ["0xaaa", "0xbbb"]
    stake_usd = 250.0
    conviction_usd = 5000
    qty = 2
    include_maker_fills = false
    "#;
        let params: Params = toml::from_str(toml_source).unwrap();
        let s = SportTaker::from_params(&params);
        assert_eq!(s.wallets, vec!["0xaaa".to_string(), "0xbbb".to_string()]);
        assert_eq!(s.stake_usd, 250.0);
        assert_eq!(s.tracker.threshold(), 5000.0);
        assert_eq!(s.qty, 2.0);
        assert!(!s.include_maker_fills);
    }

    #[test]
    fn from_params_defaults_to_the_python_constants() {
        let s = SportTaker::from_params(&Params::new());
        assert_eq!(s.stake_usd, STAKE_USD);
        assert_eq!(s.tracker.threshold(), CONVICTION_USD);
        assert_eq!(s.qty, 1.0);
        assert!(s.include_maker_fills);
        assert_eq!(s.wallets.len(), 3);
    }
}
