//! `sport_taker_run` — drive [`vike_backtest::SportTaker`] over the RAW on-chain Polymarket tape.
//!
//! This is the sibling of `cheap_np_run`, and it exists to answer ONE question the live paper bot
//! cannot: **is the copy edge real?** The live table (`data_polymarket.paper_sport_taker`) carries
//! two known data defects — signals emitted in batches up to 101 h after the market's last trade
//! (a dedup/high-water-mark failure), and `price_live` snapshots captured for the wrong token or a
//! post-decision moment. Both are properties of the BOT, not of the strategy. So this runner
//! rebuilds signals AND fills from `data_polymarket.polymarket_trades` — the on-chain tape, which
//! neither defect can touch — over the wallets' full history rather than the bot's 36 days.
//!
//! # Two passes, and why
//!
//! 1. **`--scan-only`** folds the three wallets' whole BUY tape through the pure
//!    [`vike_backtest::SignalTracker`] and writes every conviction crossing to `--triggers-out`.
//!    This pass needs ONLY the wallet tape (~137 k rows), and its output is what tells the
//!    operator which tokens' public tape to export next. It is also the signal core's ACCURACY
//!    check: its `(wallet, cid, oi, ideal_price, trigger_buy_usd)` rows are directly comparable
//!    against the live table's own — those three columns are computed before either defect.
//! 2. The full run replays, per `condition_id`, the wallet SIGNAL series + the token's public
//!    MARKET tape through the real [`vike_backtest::StrategyEngine`], settling at the on-chain
//!    payout, once per detection delay δ.
//!
//! # Two latency axes that ADD: detection δ, and the VENUE HOLD
//!
//! δ (below) is how long it takes a copier to LEARN of the wallet's fill. It is not the only
//! delay a copy order pays. Polymarket ALSO holds an incoming order itself, for a duration the
//! venue declares PER MARKET (`seconds_delay` / `sd`, and the separate `itode` 250 ms flag — see
//! `vike_polymarket::taker_hold`). The two are serial and independent: the order is sent at
//! `signal_ts + δ` and the matching engine does not look at it until `signal_ts + δ + hold`.
//!
//! `--holds FILE` (a `condition_id<TAB>hold_ms` TSV, fetched off the venue's own market payload)
//! wires that in through the engine's real mechanism rather than by inflating δ: the file becomes
//! an [`vike_backtest::EngineParams::properties`] source declaring each market's
//! `SymbolProperties::taker_hold_ms`, which the latency gate turns into the per-symbol hold table
//! it adds ON TOP of the model's entry leg. Without `--holds` nothing is installed and the run is
//! byte-identical to the pre-hold one.
//!
//! ⚠ **What the hold's OTHER semantics cost, and why this tape cannot price them.** The venue
//! holds the order un-cancellable, then re-validates it and either matches it OR RESTS it on the
//! book. A rest is neither a fill nor a miss. There is no order-book history for these markets
//! (see the fill-model note below), so a rest is UNOBSERVABLE here: this model can only say the
//! order fills at the first taker print at or after `signal_ts + δ + hold`, or misses. Read the
//! filled counts as an UPPER bound on matched-immediately.
//!
//! ⚠ **Building the `--holds` file: read the CLOB endpoints, NEVER Gamma.** Cross-checked over 200
//! of this tape's own markets (2026-07-23), `/clob-markets/{cid}`'s `sd` and `/markets/{cid}`'s
//! `seconds_delay` agreed **200 of 200**, while Gamma's `secondsDelay` agreed on only **32** — it
//! returns null where the CLOB reports a nonzero delay (168 of 200), so a Gamma-sourced file
//! silently under-reports every 1 s esports market as no hold at all. A second, independent sweep
//! over 501 markets found the same split (501/501 CLOB-vs-CLOB, 66/501 Gamma).
//!
//! ⚠ **The hold is TIME-VARYING, so resolve it PER `condition_id` — never per league.** Across the
//! 3,398 markets behind this tape, `seconds_delay` for one and the same league moves: cs2 reads 3
//! on 342 older markets and 1 on 238 newer ones; nba reads 3 on 493, 1 on 307 and 0 on 79, and the
//! nba game markets in the June–July window read **0** while the nba book open TODAY declares
//! **3**. Settlement does not wipe the field (settled cs2/lol still report 1), so these are real
//! regime changes, not decay. Substituting today's open-market league value for a historical
//! market's own value is therefore a fabrication — which is the same rule
//! `vike_polymarket::taker_hold`'s module doc states for the `kind=properties` tape.
//!
//! # δ — detection delay
//!
//! The live bot laddered `ideal → live → +2 s → +5 s`. On its own clean rows the +2 s and +5 s
//! snapshots move the price by at most 0.0018 and are IDENTICAL to the live snapshot 99.6 % of the
//! time on the CS2 wallet, while `ideal → live` costs 0.008–0.047. Sports markets do not move in a
//! five-second window; ALL of the copy cost is incurred before the copier's first look. So the
//! ladder here is δ — the gap between the copied wallet's on-chain fill and the moment a copier
//! could plausibly know about it — and it is modelled as real order latency
//! ([`vike_backtest::EngineParams::latency_model`]): the BUY becomes visible at `signal_ts + δ` and
//! fills at the next print of the token's own tape at or after that stamp, or NOT AT ALL if the
//! market resolves first. The miss is the part a SQL haircut cannot express, and it is counted.
//!
//! # The fill-model assumption, stated plainly
//!
//! **There is no order-book history for sports markets.** `polymarket_snapshots` covers only
//! long-lived markets at a 300 s cadence and has zero rows for these. So the fill is modelled from
//! the TAPE: the price of the next TAKER BUY print on that token at or after `signal_ts + δ`. That
//! is a price somebody actually paid to buy that token at that moment — the same technique
//! `cheap_np`'s latency lanes use. It is an approximation in both directions: it ignores the
//! copier's own impact (optimistic) and it cannot see a resting ask that nobody lifted
//! (pessimistic). `--exclude-self` additionally drops prints made by the copied wallets
//! themselves, which is the conservative reading — a copier cannot expect to fill alongside the
//! very order it is reacting to.
//!
//! # Fees
//!
//! ZERO, matching the Python oracle (`common.paper.settle_pnl` charges none). Polymarket's sports
//! markets took no taker fee over this history; if that changes, the `p(1−p)` curve belongs here,
//! not in `EngineParams::fee_rate` (see `cheap_np_run`'s G7 note on why the engine seam cannot
//! express it).
//!
//! # Inputs (all TSV with a header row, exported SELECT-only from the latency box)
//!
//! ```sql
//! -- --wallet-buys
//! SELECT toUnixTimestamp64Milli(ts) AS ts_ms, proxy_wallet, condition_id, asset,
//!        outcome_index, role, size, price, tx_hash, log_index, slug
//! FROM data_polymarket.polymarket_trades
//! WHERE proxy_wallet IN (...) AND side = 'BUY'
//! ORDER BY ts_ms, tx_hash, log_index FORMAT TabSeparatedWithNames
//!
//! -- --tape  (only the tokens --scan-only found; a bounded window after each trigger)
//! SELECT toUnixTimestamp64Milli(ts) AS ts_ms, condition_id, outcome_index, price, size, is_self
//! FROM ... WHERE side = 'BUY' AND role = 'taker' ... FORMAT TabSeparatedWithNames
//!
//! -- --resolutions
//! SELECT condition_id, winning_index, toUnixTimestamp64Milli(ts) AS res_ms
//! FROM data_polymarket.polymarket_resolutions_onchain
//! WHERE winning_index >= 0 FORMAT TabSeparatedWithNames
//! ```
//!
//! ```sh
//! sport_taker_run --scan-only --wallet-buys buys.tsv --triggers-out triggers.tsv
//! sport_taker_run --wallet-buys buys.tsv --tape tape.tsv --resolutions res.tsv \
//!     --delays-ms 0,5000,15000,30000,60000 --bets-out bets.tsv [--exclude-self] \
//!     [--from 2026-06-10] [--to 2026-07-23] [--no-maker-fills] [--json]
//! ```

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use vike_backtest::binutil::{arg, has_flag};
use vike_backtest::latency::LatencyModelKind;
use vike_backtest::sport_taker::{
    flat_stake_pnl, market_symbol, signal_symbol, SignalTracker, SportTaker, WalletBuy,
    CONVICTION_USD, STAKE_USD, WALLETS,
};
use vike_backtest::{EngineParams, StrategyEngine, Tick};
use vike_model::time::{days_from_civil, parse_ymd};
use vike_model::{py_sum, Bar, SymbolProperties, TradeTick};

/// The venue key the hold table is resolved under — the `EngineParams::properties` seam takes
/// `(venue, symbol, ts)`, and this runner replays exactly one venue.
const VENUE: &str = "polymarket";

// -------------------------------------------------------------------------------------------
// tiny TSV reader (header-name driven, like vike-backfill's csvutil::Header)
// -------------------------------------------------------------------------------------------

struct Tsv {
    cols: HashMap<String, usize>,
    rows: Vec<Vec<String>>,
}

impl Tsv {
    fn read(path: &Path) -> Result<Tsv, String> {
        let raw = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut lines = raw.lines();
        let header = lines.next().ok_or_else(|| format!("{}: empty", path.display()))?;
        let cols: HashMap<String, usize> =
            header.split('\t').enumerate().map(|(i, c)| (c.trim().to_string(), i)).collect();
        let rows: Vec<Vec<String>> = lines
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.split('\t').map(str::to_string).collect())
            .collect();
        Ok(Tsv { cols, rows })
    }

    fn idx(&self, name: &str) -> Result<usize, String> {
        self.cols.get(name).copied().ok_or_else(|| format!("missing column {name:?}"))
    }
}

fn get_str(row: &[String], i: usize) -> &str {
    row.get(i).map(String::as_str).unwrap_or("")
}

fn get_f64(row: &[String], i: usize) -> f64 {
    get_str(row, i).parse().unwrap_or(0.0)
}

fn get_i64(row: &[String], i: usize) -> i64 {
    get_str(row, i).parse().unwrap_or(0)
}

// -------------------------------------------------------------------------------------------
// inputs
// -------------------------------------------------------------------------------------------

/// One of the copied wallets' BUY fills, straight off `polymarket_trades`.
struct Buy {
    ts_ms: i64,
    wallet: String,
    condition_id: String,
    asset: String,
    outcome_index: u8,
    is_maker: bool,
    size: f64,
    price: f64,
    tx_hash: String,
    slug: String,
}

/// One public taker-BUY print on a token.
struct Print {
    ts_ms: i64,
    condition_id: String,
    outcome_index: u8,
    price: f64,
    /// The print was made by one of the copied wallets (`--exclude-self` drops these).
    is_self: bool,
}

fn read_buys(path: &Path) -> Result<Vec<Buy>, String> {
    let t = Tsv::read(path)?;
    let (i_ts, i_w, i_c, i_a, i_oi, i_role, i_sz, i_px, i_tx, i_slug) = (
        t.idx("ts_ms")?,
        t.idx("proxy_wallet")?,
        t.idx("condition_id")?,
        t.idx("asset")?,
        t.idx("outcome_index")?,
        t.idx("role")?,
        t.idx("size")?,
        t.idx("price")?,
        t.idx("tx_hash")?,
        t.idx("slug")?,
    );
    let mut out: Vec<Buy> = t
        .rows
        .iter()
        .map(|r| Buy {
            ts_ms: get_i64(r, i_ts),
            wallet: get_str(r, i_w).to_string(),
            condition_id: get_str(r, i_c).to_string(),
            asset: get_str(r, i_a).to_string(),
            outcome_index: get_i64(r, i_oi) as u8,
            is_maker: get_str(r, i_role).eq_ignore_ascii_case("maker"),
            size: get_f64(r, i_sz),
            price: get_f64(r, i_px),
            tx_hash: get_str(r, i_tx).to_string(),
            slug: get_str(r, i_slug).to_string(),
        })
        .collect();
    // The tape's own order is the only honest one, and a stable sort keeps same-block fills in
    // their exported (tx, log_index) order.
    out.sort_by_key(|b| b.ts_ms);
    Ok(out)
}

fn read_tape(path: &Path) -> Result<Vec<Print>, String> {
    let t = Tsv::read(path)?;
    let (i_ts, i_c, i_oi, i_px) =
        (t.idx("ts_ms")?, t.idx("condition_id")?, t.idx("outcome_index")?, t.idx("price")?);
    let i_self = t.idx("is_self").ok();
    let mut out: Vec<Print> = t
        .rows
        .iter()
        .map(|r| Print {
            ts_ms: get_i64(r, i_ts),
            condition_id: get_str(r, i_c).to_string(),
            outcome_index: get_i64(r, i_oi) as u8,
            price: get_f64(r, i_px),
            is_self: i_self.map(|i| get_str(r, i) == "1").unwrap_or(false),
        })
        .collect();
    out.sort_by_key(|p| p.ts_ms);
    Ok(out)
}

/// `condition_id -> venue-declared taker hold, ms` (`--holds`).
///
/// The file is what the VENUE says about each market — `seconds_delay * 1000`, or 250 for an
/// `itode` market — fetched per condition_id off `/clob-markets/{cid}`. A condition_id absent from
/// the file resolves to NO hold, which is the `SymbolProperties` absent convention (`0`), not a
/// guess: an unknown market must not be silently assigned some other market's number.
fn read_holds(path: &Path) -> Result<HashMap<String, u32>, String> {
    let t = Tsv::read(path)?;
    let (i_c, i_h) = (t.idx("condition_id")?, t.idx("hold_ms")?);
    Ok(t.rows
        .iter()
        .map(|r| (get_str(r, i_c).to_string(), get_i64(r, i_h).clamp(0, 3_600_000) as u32))
        .collect())
}

/// `condition_id -> (winning_index, resolution ts ms)`.
fn read_resolutions(path: &Path) -> Result<HashMap<String, (i64, i64)>, String> {
    let t = Tsv::read(path)?;
    let (i_c, i_w, i_ts) = (t.idx("condition_id")?, t.idx("winning_index")?, t.idx("res_ms")?);
    Ok(t.rows
        .iter()
        .map(|r| (get_str(r, i_c).to_string(), (get_i64(r, i_w), get_i64(r, i_ts))))
        .collect())
}

// -------------------------------------------------------------------------------------------
// bet-type taxonomy (derived from the slug, NOT from the bot's table)
// -------------------------------------------------------------------------------------------

/// Classify a market by its slug suffix. `game-N` (a single game inside a series), `spread`,
/// `total`, `map-handicap` and the residual `moneyline` behave very differently — the live table
/// showed `game-N` losing and moneyline winning, so the split has to be reproducible from data
/// the bot never touched.
fn bet_type(slug: &str) -> &'static str {
    let s = slug.to_ascii_lowercase();
    if s.contains("-map-handicap") {
        "map-handicap"
    } else if s.contains("-spread") {
        "spread"
    } else if s.contains("-total") {
        "total"
    } else if s
        .rsplit('-')
        .next()
        .map(|last| {
            last.starts_with("game")
                && last[4..].chars().all(|c| c.is_ascii_digit())
                && last.len() > 4
        })
        .unwrap_or(false)
    {
        "game-N"
    } else {
        "moneyline"
    }
}

// -------------------------------------------------------------------------------------------
// pass 1 — the pure signal scan
// -------------------------------------------------------------------------------------------

struct Trigger {
    ts_ms: i64,
    wallet: String,
    condition_id: String,
    outcome_index: u8,
    asset: String,
    slug: String,
    trigger_buy_usd: f64,
    ideal_price: f64,
}

/// Fold the whole wallet tape through the pure tracker, honouring the Python's exact dedup key.
fn scan(buys: &[Buy], threshold: f64, include_maker: bool) -> (Vec<Trigger>, u64) {
    let mut tracker = SignalTracker::new(threshold);
    let mut out = Vec::new();
    for b in buys {
        if b.is_maker && !include_maker {
            continue;
        }
        let wb = WalletBuy {
            wallet: &b.wallet,
            condition_id: &b.condition_id,
            outcome_index: b.outcome_index,
            asset: &b.asset,
            slug: &b.slug,
            size: b.size,
            price: b.price,
            tx_hash: &b.tx_hash,
        };
        if let Some(s) = tracker.ingest_one(&wb, true) {
            out.push(Trigger {
                ts_ms: b.ts_ms,
                wallet: s.copied_wallet,
                condition_id: s.condition_id,
                outcome_index: s.outcome_index,
                asset: s.asset,
                slug: s.slug,
                trigger_buy_usd: s.trigger_buy_usd,
                ideal_price: s.ideal_price,
            });
        }
    }
    let collisions = tracker.dedup_collisions();
    (out, collisions)
}

// -------------------------------------------------------------------------------------------
// aggregation
// -------------------------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
struct Agg {
    bets: usize,
    unfilled: usize,
    wins: f64,
    px_sum: f64,
    pnl: f64,
}

impl Agg {
    fn push(&mut self, price: f64, payout: f64) {
        self.bets += 1;
        self.wins += payout;
        self.px_sum += price;
        self.pnl += flat_stake_pnl(price, payout, STAKE_USD);
    }
    fn line(&self, label: &str) -> String {
        let n = self.bets.max(1) as f64;
        format!(
            "{label:<44} bets={:<5} unfilled={:<4} win={:.3} px={:.4} pnl={:>10.2} \
             per_bet={:>7.2}",
            self.bets,
            self.unfilled,
            self.wins / n,
            self.px_sum / n,
            self.pnl,
            self.pnl / n,
        )
    }
}

/// One settled copy bet at one δ — the row `--bets-out` writes.
struct BetRow {
    delay_ms: i64,
    wallet: String,
    condition_id: String,
    outcome_index: u8,
    slug: String,
    bet_type: &'static str,
    /// The venue hold actually served for this market, ms (`0` = none / no `--holds`).
    hold_ms: u32,
    ts_signal: i64,
    ideal_price: f64,
    trigger_buy_usd: f64,
    fill_price: f64,
    filled: bool,
    payout: f64,
}

// -------------------------------------------------------------------------------------------
// the run
// -------------------------------------------------------------------------------------------

#[allow(clippy::too_many_arguments)]
fn run_condition(
    cid: &str,
    buys: &[&Buy],
    prints: &[&Print],
    winning_index: i64,
    res_ms: i64,
    delays: &[i64],
    include_maker: bool,
    exclude_self: bool,
    // Conviction threshold (`--conviction-usd`) the strategy arms its tracker at — the SAME value
    // the pre-pass `scan()` uses. Was previously ignored here (`SportTaker::default()` hardcoded
    // 2000), so the flag only affected the scan/triggers count, never the backtested bets.
    threshold: f64,
    // `Some(ms)` installs this market's VENUE HOLD through the properties seam; `None` (no
    // `--holds`) installs no source at all and keeps the run byte-identical to the pre-hold one.
    hold_ms: Option<u32>,
    slug_of: &HashMap<(String, u8), String>,
    out: &mut Vec<BetRow>,
) {
    // --- streams ------------------------------------------------------------------------------
    // SIGNAL streams first (lower stream index) so a crossing is dispatched BEFORE the same-block
    // market print it could copy — the honest δ = 0 bound.
    let mut sig: BTreeMap<(String, u8), Vec<Tick>> = BTreeMap::new();
    for b in buys {
        if b.is_maker && !include_maker {
            continue;
        }
        let sym = signal_symbol(&b.wallet, cid, b.outcome_index);
        sig.entry((b.wallet.clone(), b.outcome_index)).or_default().push(Tick::Trade(TradeTick {
            ts: b.ts_ms,
            local_ts: 0,
            price: b.price,
            size: b.size,
            is_buyer_maker: b.is_maker,
            symbol: sym,
        }));
    }
    let mut mkt: BTreeMap<u8, Vec<Tick>> = BTreeMap::new();
    for p in prints {
        if exclude_self && p.is_self {
            continue;
        }
        let sym = market_symbol(cid, p.outcome_index);
        mkt.entry(p.outcome_index).or_default().push(Tick::Trade(TradeTick {
            ts: p.ts_ms,
            local_ts: 0,
            price: p.price,
            size: 0.0,
            is_buyer_maker: false,
            symbol: sym,
        }));
    }

    let mut series: Vec<(String, Vec<Tick>)> = Vec::new();
    for ((w, oi), ticks) in &sig {
        series.push((signal_symbol(w, cid, *oi), ticks.clone()));
    }
    for (oi, ticks) in &mkt {
        series.push((market_symbol(cid, *oi), ticks.clone()));
    }
    // Every token of this market is registered even if it never printed — a signal on a token
    // with no tape must still be able to submit (and then miss), not be silently dropped as an
    // unknown symbol.
    let mut symbols: Vec<(String, Vec<Bar>)> =
        series.iter().map(|(s, _)| (s.clone(), Vec::new())).collect();
    for (_, oi) in sig.keys() {
        let m = market_symbol(cid, *oi);
        if !symbols.iter().any(|(s, _)| *s == m) {
            symbols.push((m, Vec::new()));
        }
    }

    for &delay in delays {
        let wi = winning_index;
        let cid_owned = cid.to_string();
        let resolution = Box::new(move |sym: &str, ts: i64| -> Option<f64> {
            if ts < res_ms {
                return None;
            }
            let (c, oi) = sym.rsplit_once('#')?;
            if c != cid_owned {
                return None;
            }
            let oi: i64 = oi.parse().ok()?;
            Some(f64::from(u8::from(oi == wi)))
        });
        let mut strat = SportTaker::new(threshold);
        strat.include_maker_fills = include_maker;
        // The VENUE HOLD rides on the engine's own per-symbol hold table, NOT on δ — the two are
        // separate delays a live order pays in series, and keeping them separate is the whole
        // point of the grid this runner prints. The properties source declares only
        // `taker_hold_ms`; every other field stays `0.0`, which is the inert grid (`nz_step`
        // rounds by nothing, the zero min_qty/min_notional floors never trip), so installing it
        // changes the fill path in exactly one way: the hold.
        //
        // A SIGNAL symbol (`wallet@cid#oi`) is never traded, and it deliberately gets no hold —
        // the hold belongs to the market being bought, and giving the signal series one would put
        // a nonzero entry in the table for a symbol no order is ever pushed against.
        #[allow(clippy::type_complexity)] // the `EngineParams::properties` seam's own signature
        let props: Option<
            Arc<dyn Fn(&str, &str, i64) -> Option<SymbolProperties> + Send + Sync>,
        > = hold_ms.filter(|h| *h > 0).map(|h| {
            let grid = SymbolProperties { taker_hold_ms: h, ..Default::default() };
            Arc::new(move |_venue: &str, sym: &str, _ts: i64| (!sym.contains('@')).then_some(grid))
                as Arc<_>
        });
        // Arming the gate is what serves the hold, so it must be armed for a zero-δ held run too
        // (a constant-0 model is a no-op on its own — the pre-hold `delay > 0` shape).
        let armed = delay > 0 || props.is_some();
        let mut engine = StrategyEngine::new(
            symbols.clone(),
            strat,
            EngineParams {
                cash: 1_000_000.0,
                // Zero, matching the Python oracle — see the module doc's fee note.
                fee_rate: 0.0,
                slippage: 0.0,
                resolution: Some(resolution),
                // The tape stops at the trading halt, well before the on-chain resolution posts.
                resolution_end_ts: Some(res_ms),
                latency_model: armed
                    .then(|| LatencyModelKind::constant(delay.saturating_mul(1_000_000), 0)),
                default_venue: props.is_some().then(|| VENUE.to_string()),
                properties: props,
                ..Default::default()
            },
        );
        let res = engine.run_ticks(&series);
        // Map each fired crossing to its executed leg. The engine books ONE long trade per filled
        // entry and the strategy fires at most one entry per (wallet, cid, oi), so pairing by
        // market symbol in fire order is exact; a fired crossing with no matching trade MISSED.
        let mut fills: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
        for t in &res.trades {
            if !t.is_long {
                continue;
            }
            fills.entry(t.symbol.clone()).or_default().push((t.entry_price, t.exit_price));
        }
        for f in &engine.strategy.fired {
            let slot = fills.get_mut(&f.market).and_then(|v| {
                if v.is_empty() {
                    None
                } else {
                    Some(v.remove(0))
                }
            });
            let slug = slug_of
                .get(&(f.signal.condition_id.clone(), f.signal.outcome_index))
                .cloned()
                .unwrap_or_default();
            let (fill_price, payout, filled) = match slot {
                Some((px, po)) => (px, po, true),
                None => (0.0, 0.0, false),
            };
            out.push(BetRow {
                delay_ms: delay,
                wallet: f.signal.copied_wallet.clone(),
                condition_id: f.signal.condition_id.clone(),
                outcome_index: f.signal.outcome_index,
                bet_type: bet_type(&slug),
                slug,
                hold_ms: hold_ms.unwrap_or(0),
                ts_signal: f.ts,
                ideal_price: f.signal.ideal_price,
                trigger_buy_usd: f.signal.trigger_buy_usd,
                fill_price,
                filled,
                payout,
            });
        }
    }
}

/// Midnight-UTC epoch milliseconds of a `YYYY-MM-DD` day — the shared Hinnant calendar math
/// (`vike_model::time`), exactly as `cheap_np_run`'s `day_start_secs` uses it, replacing the
/// inline civil-from-days copy this bin used to carry.
fn day_ms(s: &str) -> Option<i64> {
    let (y, m, d) = parse_ymd(s).ok()?;
    Some(days_from_civil(y, m, d) * 86_400_000)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    let Some(buys_path) = arg(&args, "--wallet-buys") else {
        eprintln!(
            "usage: sport_taker_run --wallet-buys buys.tsv \
             [--scan-only --triggers-out FILE] \
             [--tape tape.tsv --resolutions res.tsv --delays-ms 0,5000,... --bets-out FILE] \
             [--holds holds.tsv] [--exclude-self] [--no-maker-fills] [--conviction-usd 2000] [--from YYYY-MM-DD] \
             [--to YYYY-MM-DD] [--json]"
        );
        return ExitCode::FAILURE;
    };
    let include_maker = !has_flag(&args, "--no-maker-fills");
    let exclude_self = has_flag(&args, "--exclude-self");
    let threshold =
        arg(&args, "--conviction-usd").and_then(|v| v.parse().ok()).unwrap_or(CONVICTION_USD);
    let from_ms = arg(&args, "--from").and_then(|s| day_ms(&s));
    let to_ms = arg(&args, "--to").and_then(|s| day_ms(&s)).map(|m| m + 86_400_000);

    let buys = match read_buys(Path::new(&buys_path)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("--wallet-buys: {e}");
            return ExitCode::FAILURE;
        }
    };
    let watched: HashSet<&str> = WALLETS.iter().copied().collect();
    let unknown: HashSet<&str> =
        buys.iter().map(|b| b.wallet.as_str()).filter(|w| !watched.contains(w)).collect();
    if !unknown.is_empty() {
        eprintln!("warning: {} wallet(s) in the tape are not in WALLETS", unknown.len());
    }
    eprintln!("sport_taker_run: {} wallet BUY fills, include_maker={include_maker}", buys.len());

    let (triggers, collisions) = scan(&buys, threshold, include_maker);
    eprintln!(
        "  scan: {} conviction crossings, {} fill(s) collapsed by the Python dedup key",
        triggers.len(),
        collisions
    );

    if has_flag(&args, "--scan-only") {
        let mut s = String::from(
            "ts_ms\twallet\tcondition_id\toutcome_index\tasset\tslug\ttrigger_buy_usd\tideal_price\n",
        );
        for t in &triggers {
            let _ = writeln!(
                s,
                "{}\t{}\t{}\t{}\t{}\t{}\t{:.4}\t{:.6}",
                t.ts_ms,
                t.wallet,
                t.condition_id,
                t.outcome_index,
                t.asset,
                t.slug,
                t.trigger_buy_usd,
                t.ideal_price
            );
        }
        match arg(&args, "--triggers-out") {
            Some(p) => {
                if let Err(e) = fs::write(&p, s) {
                    eprintln!("--triggers-out {p}: {e}");
                    return ExitCode::FAILURE;
                }
                eprintln!("  wrote {} trigger rows to {p}", triggers.len());
            }
            None => print!("{s}"),
        }
        return ExitCode::SUCCESS;
    }

    let (Some(tape_path), Some(res_path)) = (arg(&args, "--tape"), arg(&args, "--resolutions"))
    else {
        eprintln!("--tape and --resolutions are required unless --scan-only");
        return ExitCode::FAILURE;
    };
    let prints = match read_tape(Path::new(&tape_path)) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("--tape: {e}");
            return ExitCode::FAILURE;
        }
    };
    let resolutions = match read_resolutions(Path::new(&res_path)) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("--resolutions: {e}");
            return ExitCode::FAILURE;
        }
    };
    let holds = match arg(&args, "--holds") {
        Some(p) => match read_holds(Path::new(&p)) {
            Ok(h) => Some(h),
            Err(e) => {
                eprintln!("--holds: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    let delays: Vec<i64> = arg(&args, "--delays-ms")
        .map(|s| s.split(',').filter_map(|p| p.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![0]);
    eprintln!(
        "  tape: {} prints, {} resolutions, delays_ms={delays:?}, exclude_self={exclude_self}",
        prints.len(),
        resolutions.len()
    );

    // Only condition_ids that actually produced a crossing are replayed.
    let mut want: HashSet<&str> = HashSet::new();
    let mut slug_of: HashMap<(String, u8), String> = HashMap::new();
    let mut ideal_of: HashMap<(String, String, u8), (f64, f64, i64)> = HashMap::new();
    for t in &triggers {
        want.insert(t.condition_id.as_str());
        slug_of.insert((t.condition_id.clone(), t.outcome_index), t.slug.clone());
        ideal_of.insert(
            (t.wallet.clone(), t.condition_id.clone(), t.outcome_index),
            (t.ideal_price, t.trigger_buy_usd, t.ts_ms),
        );
    }

    let mut by_cid_buys: HashMap<&str, Vec<&Buy>> = HashMap::new();
    for b in &buys {
        if want.contains(b.condition_id.as_str()) {
            by_cid_buys.entry(b.condition_id.as_str()).or_default().push(b);
        }
    }
    let mut by_cid_prints: HashMap<&str, Vec<&Print>> = HashMap::new();
    for p in &prints {
        if want.contains(p.condition_id.as_str()) {
            by_cid_prints.entry(p.condition_id.as_str()).or_default().push(p);
        }
    }

    let mut bets: Vec<BetRow> = Vec::new();
    let (mut unresolved, mut bad_res, mut no_tape) = (0usize, 0usize, 0usize);
    let mut hold_missing = 0usize;
    let mut cids: Vec<&str> = want.iter().copied().collect();
    cids.sort_unstable();
    for cid in cids {
        let Some(&(wi, res_ms)) = resolutions.get(cid) else {
            unresolved += 1;
            continue;
        };
        if !(0..=1).contains(&wi) {
            // A market whose outcome this binary model cannot express is EXCLUDED, never coerced
            // into "both sides lose" (the same rule `cheap_np_run` applies).
            bad_res += 1;
            continue;
        }
        let Some(cbuys) = by_cid_buys.get(cid) else { continue };
        let empty: Vec<&Print> = Vec::new();
        let cprints = by_cid_prints.get(cid).unwrap_or(&empty);
        if cprints.is_empty() {
            no_tape += 1;
        }
        // `--holds` given but this condition_id absent from it = the venue no longer answers for
        // that market. It is counted and reported, NEVER back-filled from a sibling's number.
        let hold_ms = holds.as_ref().map(|h| {
            let v = h.get(cid).copied();
            if v.is_none() {
                hold_missing += 1;
            }
            v.unwrap_or(0)
        });
        run_condition(
            cid,
            cbuys,
            cprints,
            wi,
            res_ms,
            &delays,
            include_maker,
            exclude_self,
            threshold,
            hold_ms,
            &slug_of,
            &mut bets,
        );
    }
    eprintln!(
        "  replayed {} bet-rows; excluded: {unresolved} unresolved cid(s), {bad_res} \
         non-binary resolution(s), {no_tape} cid(s) with no public tape",
        bets.len()
    );
    if holds.is_some() {
        eprintln!("  venue holds: {hold_missing} replayed cid(s) had NO declared hold on file");
    }

    // --- window filter (applied to the SIGNAL stamp, so both windows use the same bets) --------
    let in_window =
        |ts: i64| from_ms.map(|f| ts >= f).unwrap_or(true) && to_ms.map(|t| ts < t).unwrap_or(true);
    let sel: Vec<&BetRow> = bets.iter().filter(|b| in_window(b.ts_signal)).collect();

    // --- report --------------------------------------------------------------------------------
    let mut by_wallet_delay: BTreeMap<(String, i64), Agg> = BTreeMap::new();
    let mut by_wallet_type: BTreeMap<(String, &'static str, i64), Agg> = BTreeMap::new();
    let mut ideal: BTreeMap<String, Agg> = BTreeMap::new();
    let mut seen_ideal: HashSet<(String, String, u8)> = HashSet::new();
    for b in &sel {
        let e = by_wallet_delay.entry((b.wallet.clone(), b.delay_ms)).or_default();
        if b.filled {
            e.push(b.fill_price, b.payout);
        } else {
            e.unfilled += 1;
        }
        let e = by_wallet_type.entry((b.wallet.clone(), b.bet_type, b.delay_ms)).or_default();
        if b.filled {
            e.push(b.fill_price, b.payout);
        } else {
            e.unfilled += 1;
        }
        // The IDEAL lane is δ-invariant, so it is booked once per bet (not once per δ) — and it
        // uses the settled payout of whichever δ actually filled, or the market's own outcome.
        let k = (b.wallet.clone(), b.condition_id.clone(), b.outcome_index);
        if seen_ideal.insert(k) && b.filled {
            ideal.entry(b.wallet.clone()).or_default().push(b.ideal_price, b.payout);
        }
    }

    println!("== IDEAL lane (wallet VWAP — UNREACHABLE by a copier) ==");
    for (w, a) in &ideal {
        println!("{}", a.line(&format!("{}  ideal", &w[..10])));
    }
    println!("\n== by wallet x detection delay ==");
    for ((w, d), a) in &by_wallet_delay {
        println!("{}", a.line(&format!("{}  d={}ms", &w[..10], d)));
    }
    if holds.is_some() {
        println!("\n== by wallet x served venue hold x detection delay ==");
        let mut by_hold: BTreeMap<(String, u32, i64), Agg> = BTreeMap::new();
        for b in &sel {
            let e = by_hold.entry((b.wallet.clone(), b.hold_ms, b.delay_ms)).or_default();
            if b.filled {
                e.push(b.fill_price, b.payout);
            } else {
                e.unfilled += 1;
            }
        }
        for ((w, h, d), a) in &by_hold {
            println!("{}", a.line(&format!("{}  hold={h}ms d={d}ms", &w[..10])));
        }
    }
    println!("\n== by wallet x bet type x detection delay ==");
    for ((w, bt, d), a) in &by_wallet_type {
        println!("{}", a.line(&format!("{}  {bt:<13} d={}ms", &w[..10], d)));
    }
    // Headline total at the SMALLEST configured δ — not a hard-coded `0`, which printed a
    // meaningless 0.00 whenever the operator swept a ladder that did not include zero.
    let d_min = by_wallet_delay.keys().map(|(_, d)| *d).min().unwrap_or(0);
    let total: f64 =
        py_sum(by_wallet_delay.iter().filter(|((_, d), _)| *d == d_min).map(|(_, a)| a.pnl));
    println!("\ntotal pnl across wallets at d={d_min}ms: {total:.2}");

    if let Some(p) = arg(&args, "--bets-out") {
        let mut s = String::from(
            "delay_ms\thold_ms\twallet\tcondition_id\toutcome_index\tslug\tbet_type\tts_signal\t\
             ideal_price\ttrigger_buy_usd\tfill_price\tfilled\tpayout\tpnl\n",
        );
        for b in &bets {
            let pnl =
                if b.filled { flat_stake_pnl(b.fill_price, b.payout, STAKE_USD) } else { 0.0 };
            let _ = writeln!(
                s,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{:.4}\t{:.6}\t{}\t{}\t{:.4}",
                b.delay_ms,
                b.hold_ms,
                b.wallet,
                b.condition_id,
                b.outcome_index,
                b.slug,
                b.bet_type,
                b.ts_signal,
                b.ideal_price,
                b.trigger_buy_usd,
                b.fill_price,
                u8::from(b.filled),
                b.payout,
                pnl
            );
        }
        if let Err(e) = fs::write(&p, s) {
            eprintln!("--bets-out {p}: {e}");
            return ExitCode::FAILURE;
        }
        eprintln!("  wrote {} bet rows to {p}", bets.len());
    }

    if has_flag(&args, "--json") {
        let rows: Vec<serde_json::Value> = by_wallet_delay
            .iter()
            .map(|((w, d), a)| {
                let n = a.bets.max(1) as f64;
                serde_json::json!({
                    "wallet": w, "delay_ms": d, "bets": a.bets, "unfilled": a.unfilled,
                    "win_rate": a.wins / n, "avg_fill_price": a.px_sum / n,
                    "total_pnl": a.pnl, "pnl_per_bet": a.pnl / n,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "triggers": triggers.len(), "dedup_collisions": collisions,
                "stake_usd": STAKE_USD, "conviction_usd": threshold,
                "exclude_self": exclude_self, "include_maker_fills": include_maker,
                "venue_holds_applied": holds.is_some(),
                "cids_without_declared_hold": hold_missing,
                "by_wallet_delay": rows,
            }))
            .unwrap_or_default()
        );
    }
    ExitCode::SUCCESS
}
