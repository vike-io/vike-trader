//! `covered_call` — the weekly covered call on recorded option chains (the book's options chapter,
//! "Backtesting option strategies"): hold the underlying, and each Friday at 08:00 UTC sell the call that
//! expires about a week later whose delta is nearest `target_delta`, holding it to its expiry. On deribit
//! history: the hourly chains derived from the venue's public trade log
//! (`vike-cli data history fetch deribit:BTC:option-trades`), the underlying's hourly bars beside them.
//!
//! What it does, in two halves:
//!
//! 1. **Look**, point in time: at each sale instant `t` it reads the chain as it stood at `t`
//!    (`StudyContext::chain_as_of`: each option's last row in `[t - lookback, t]`, nothing later) and the
//!    underlying's last bar CLOSED by `t`. The week's CANDIDATES are the live calls of the expiry nearest
//!    `t + tenor`; each one's IV is its row's (the venue's) when the row carries a finite positive one,
//!    else inverted from its mark (`vike_options::quote_iv`), and its delta is `vike_options::delta_at_quote`
//!    at that IV. The table reports the candidate nearest `target_delta` as the study saw it at `t`.
//! 2. **Run** the candidates through the event simulator: ONE backtest (`StudyContext::sim`) whose profile
//!    names them as `[data.options]` legs and the shipped strategy file `options/covered_call.rs`, which
//!    picks by delta itself, on the quotes it holds at its first event after `t` (the same Black–Scholes).
//!    A call is sold at its NEXT priced chain row (no fill without a price), and settled at its expiry to
//!    cash at its intrinsic value against the underlying.
//!
//! Recipe (no default for anything that names a market):
//!
//! ```toml
//! venue = "deribit"                     # the chain's venue
//! chain = "BTC"                         # the kind=chain series (deribit's coin book)
//! underlying_venue = "deribit"          # the underlying's series: a LINEAR one (the legs are dollars)
//! underlying = "BTCUSDC.P"
//! interval = "1h"                       # its stored bars
//! target_delta = 0.3                    # optional: the delta sought
//! tenor_days = 7                        # optional: the expiry sought is the sale + this
//! lookback_hours = 24                   # optional: how old a chain row may be when choosing
//! quantity = 1.0                        # optional: units of the underlying held, and of each call sold
//! cash = 1000000.0                      # optional: the run's starting cash, in dollars
//! assumed_half_spread = 0.053           # the half-spread a call trades at, a fraction of its mark
//! ```
//!
//! `assumed_half_spread` is REQUIRED on deribit history: its chains are derived from the venue's trades and
//! carry no bid or ask, and the backtest never fills a leg at the bare mark (`[data.options]`
//! `assumed_half_spread`, REVIEW-LIVE-MONEY M7). A call sells at `mark · (1 − assumed_half_spread)`, and the
//! run's data warnings say the spread was assumed. 0.053 is MEASURED: on 2026-11-17 deribit's public
//! `get_book_summary_by_currency` (BTC options) quoted the 27NOV26 calls 5% to 15% out of the money (a week
//! and a half to expiry, spot near 64,000) at half-spreads of 3.0% to 7.6% of their marks, median 5.3%. One
//! snapshot: re-measure before reading a return to the basis point. On a chain that carries quotes the key
//! is not read.
//!
//! Metrics: `weeks` (Fridays with a call to sell), `skipped` (Fridays with none), `premium` and `payout`
//! (the table's calls' marks at `t` and their intrinsic values at expiry, by hand), `mean_iv`,
//! `mean_delta`, and the engine run's `total_return`, `trades` and `win_rate`. Artifact `calls.tsv`: one
//! row per week, its call nearest the target delta at `t`.

use std::sync::Arc;

use vike_data::{ChainRow, HistoryStore, HistoryStoreError, TimestampRange};
use vike_marketdata::{Bar, L2BookUpdate, OptionKind, TickQuote, TickTrade};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 24 * HOUR_MS;
/// Day 0 of the epoch was a Thursday, so a Friday is a day number `≡ 1 (mod 7)`.
const FRIDAY: i64 = 1;
/// Deribit's weekly expiry, and so the sale instant: 08:00 UTC.
const SALE_HOUR_MS: i64 = 8 * HOUR_MS;

fn text<'a>(params: &'a toml::Value, key: &str) -> Result<&'a str, StudyError> {
    params.get(key).and_then(toml::Value::as_str).ok_or_else(|| {
        StudyError::Study(format!("the recipe must name `{key}` (no market has a default)"))
    })
}

fn number(params: &toml::Value, key: &str, default: f64) -> f64 {
    params
        .get(key)
        .and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
        .unwrap_or(default)
}

/// One Friday: the week's candidates, and the one nearest the target delta as the study saw it at `t`.
struct Chosen {
    sale_ms: i64,
    candidates: Vec<ChainRow>,
    row: ChainRow,
    spot: f64,
    iv: Option<f64>,
    delta: Option<f64>,
    /// Its intrinsic value at expiry against the underlying's last close by then, when the window
    /// reaches the expiry.
    payout: Option<f64>,
}

/// A row's IV (the venue's, else inverted from its mark) and its delta at that IV, at `now` and `spot`:
/// the platform's one Black–Scholes.
fn iv_and_delta(row: &ChainRow, now: i64, spot: f64) -> (Option<f64>, Option<f64>) {
    let years = vike_options::years_to_expiry_ms(row.expiry_ms, now);
    let (iv, mark, call) = (row.iv, row.mark, OptionKind::Call);
    (
        vike_options::quote_iv(iv, mark, spot, row.strike, years, call, 0.0),
        vike_options::delta_at_quote(iv, mark, spot, row.strike, years, call, 0.0),
    )
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = text(params, "venue")?;
    let chain = text(params, "chain")?;
    let underlying_venue = text(params, "underlying_venue")?;
    let underlying = text(params, "underlying")?;
    let interval = text(params, "interval")?;
    let target_delta = number(params, "target_delta", 0.3);
    let tenor_ms = (number(params, "tenor_days", 7.0) * DAY_MS as f64) as i64;
    let lookback_ms = (number(params, "lookback_hours", 24.0) * HOUR_MS as f64) as i64;
    let quantity = number(params, "quantity", 1.0);
    let cash = number(params, "cash", 1_000_000.0);
    let assumed_half_spread = params.get("assumed_half_spread").and_then(toml::Value::as_float);
    let Some(interval_ms) = vike_marketdata::interval::interval_ms(interval).filter(|ms| *ms > 0)
    else {
        return Err(StudyError::Study(format!("interval {interval:?} is not a bar interval")));
    };
    let window = ctx.window();
    let (Some(from), Some(to)) = (window.start, window.end) else {
        return Err(StudyError::Study("the study needs a bounded window (--from, --to)".into()));
    };
    let Some(sim) = ctx.sim() else {
        return Err(StudyError::NoSim(format!("the covered call on {venue}'s {chain} chain")));
    };

    let bars = ctx.bars(underlying_venue, underlying, interval, window)?;
    if bars.is_empty() {
        return Err(StudyError::Study(format!(
            "no {interval} bars of {underlying} on {underlying_venue} in the window: fetch them \
             (vike-cli data history fetch {underlying_venue}:{underlying}:{interval})"
        )));
    }
    // The underlying's price as known at `t`: the close of the last bar CLOSED by then.
    let spot_at =
        |t: i64| bars.iter().take_while(|b| b.timestamp + interval_ms <= t).last().map(|b| b.close);

    let mut chosen: Vec<Chosen> = Vec::new();
    let mut skipped = 0u32;
    for sale_ms in sale_instants(from, to) {
        let Some(spot) = spot_at(sale_ms) else {
            skipped += 1;
            continue;
        };
        let rows = ctx.chain_as_of(venue, chain, sale_ms, lookback_ms)?;
        let candidates = candidates(&rows, sale_ms, sale_ms + tenor_ms);
        let nearest = candidates
            .iter()
            .filter_map(|r| Some((r, iv_and_delta(r, sale_ms, spot).1?)))
            .min_by(|(a, da), (b, db)| {
                let gap = |d: f64| (d - target_delta).abs();
                gap(*da).total_cmp(&gap(*db)).then(b.strike.total_cmp(&a.strike))
            })
            .map(|(r, _)| r.clone());
        let Some(row) = nearest else {
            skipped += 1;
            continue;
        };
        let (iv, delta) = iv_and_delta(&row, sale_ms, spot);
        let payout = (row.expiry_ms <= to)
            .then(|| spot_at(row.expiry_ms))
            .flatten()
            .map(|settle| vike_options::intrinsic(OptionKind::Call, row.strike, settle));
        chosen.push(Chosen { sale_ms, candidates, row, spot, iv, delta, payout });
    }

    let mut out = StudyOutcome::new();
    out.metric("weeks", chosen.len() as f64)?;
    out.metric("skipped", f64::from(skipped))?;
    let premium = vike_math::summation::compensated_sum(
        chosen.iter().map(|c| c.row.mark.unwrap_or(0.0) * quantity),
    );
    let payout = vike_math::summation::compensated_sum(
        chosen.iter().map(|c| c.payout.unwrap_or(0.0) * quantity),
    );
    out.metric("premium", premium)?;
    out.metric("payout", payout)?;
    out.metric(
        "mean_iv",
        vike_math::statistics::mean(chosen.iter().filter_map(|c| c.iv).collect::<Vec<_>>()),
    )?;
    out.metric(
        "mean_delta",
        vike_math::statistics::mean(chosen.iter().filter_map(|c| c.delta).collect::<Vec<_>>()),
    )?;

    if !chosen.is_empty() {
        let words = ProfileWords {
            venue,
            chain,
            underlying_venue,
            underlying,
            interval,
            quantity,
            cash,
            target_delta,
            tenor_days: number(params, "tenor_days", 7.0),
            lookback_hours: number(params, "lookback_hours", 24.0),
            assumed_half_spread,
        };
        let profile = profile(words, from, to, &chosen)?;
        let store: Arc<dyn HistoryStore + Send + Sync> = Arc::new(ContextStore(ctx.clone()));
        let run = sim.run_one(&profile, store).map_err(|why| {
            StudyError::Study(format!("the covered call's backtest: {}", why.replace('\n', " ")))
        })?;
        out.metric("total_return", run.total_return)?;
        out.metric("trades", run.n_trades as f64)?;
        out.metric("win_rate", run.win_rate)?;
    }

    let mut tsv =
        String::from("sale_ms\tinstrument\texpiry_ms\tstrike\tspot\tmark\tiv\tdelta\tpayout\n");
    for c in &chosen {
        let cell = |v: Option<f64>| v.map_or_else(String::new, |v| v.to_string());
        tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.sale_ms,
            c.row.instrument,
            c.row.expiry_ms,
            c.row.strike,
            c.spot,
            cell(c.row.mark),
            cell(c.iv),
            cell(c.delta),
            cell(c.payout)
        ));
    }
    out.artifact("calls.tsv", tsv)?;
    Ok(out)
}

/// Every Friday 08:00 UTC in `[from, to)`.
fn sale_instants(from: i64, to: i64) -> Vec<i64> {
    let mut day = from.div_euclid(DAY_MS);
    while day.rem_euclid(7) != FRIDAY || day * DAY_MS + SALE_HOUR_MS < from {
        day += 1;
    }
    let mut out = Vec::new();
    while day * DAY_MS + SALE_HOUR_MS < to {
        out.push(day * DAY_MS + SALE_HOUR_MS);
        day += 7;
    }
    out
}

/// The week's candidates at `now`: the live calls with a mark, of the expiry nearest `expiry` (within a
/// day of it). Empty when there is none.
fn candidates(rows: &[ChainRow], now: i64, expiry: i64) -> Vec<ChainRow> {
    let calls = || {
        rows.iter().filter(move |r| {
            r.kind == OptionKind::Call && r.expiry_ms > now && r.mark.is_some_and(f64::is_finite)
        })
    };
    let Some(nearest_expiry) = calls()
        .map(|r| r.expiry_ms)
        .filter(|e| (e - expiry).abs() <= DAY_MS)
        .min_by_key(|e| ((e - expiry).abs(), *e))
    else {
        return Vec::new();
    };
    calls().filter(|r| r.expiry_ms == nearest_expiry).cloned().collect()
}

/// The recipe's words the profile repeats.
struct ProfileWords<'a> {
    venue: &'a str,
    chain: &'a str,
    underlying_venue: &'a str,
    underlying: &'a str,
    interval: &'a str,
    quantity: f64,
    cash: f64,
    target_delta: f64,
    tenor_days: f64,
    lookback_hours: f64,
    /// The recipe's `assumed_half_spread`, handed to `[data.options]` as it is.
    assumed_half_spread: Option<f64>,
}

/// The one backtest: the underlying from its bars, every week's candidates as `[data.options]` legs, the
/// shipped strategy file picking by delta among them at each sale instant. It pays the strategy's stated
/// commission, which `options/covered_call.rs` leaves at none: no venue fee table applies.
fn profile(
    w: ProfileWords<'_>,
    from: i64,
    to: i64,
    chosen: &[Chosen],
) -> Result<toml::Value, StudyError> {
    // Every week's candidates, each once, in the order first seen.
    let mut rows: Vec<&ChainRow> = Vec::new();
    for row in chosen.iter().flat_map(|c| &c.candidates) {
        if !rows.iter().any(|r| r.instrument == row.instrument) {
            rows.push(row);
        }
    }
    let list = |cell: &dyn Fn(&ChainRow) -> String| {
        rows.iter().map(|r| cell(r)).collect::<Vec<_>>().join(", ")
    };
    let legs = list(&|r| format!("{:?}", r.instrument));
    let sell_at: Vec<String> = chosen.iter().map(|c| c.sale_ms.to_string()).collect();
    let half_spread = w
        .assumed_half_spread
        .map_or(String::new(), |half| format!("assumed_half_spread = {half:?}\n"));
    let text = format!(
        "[data]\nkind = \"tick\"\nfrom = \"{from}\"\nto = \"{to}\"\n\n\
         [[data.series]]\nvenue = {:?}\nsymbol = {:?}\n\n\
         [data.options]\nvenue = {:?}\nchain = {:?}\nunderlying = {:?}\nunderlying_interval = {:?}\n\
         legs = [{legs}]\n{half_spread}\n\
         [engine]\ncash = {cash:?}\n\n\
         [strategy]\nfile = \"options/covered_call.rs\"\n\
         [strategy.params]\nunderlying = {:?}\nquantity = {quantity:?}\ntarget_delta = {delta:?}\n\
         tenor_days = {tenor:?}\nmax_quote_age_hours = {age:?}\n\
         calls = [{legs}]\nstrikes = [{strikes}]\nexpiries = [{expiries}]\nsell_at = [{sell_at}]\n",
        w.underlying_venue,
        w.underlying,
        w.venue,
        w.chain,
        w.underlying,
        w.interval,
        w.underlying,
        cash = w.cash,
        quantity = w.quantity,
        delta = w.target_delta,
        tenor = w.tenor_days,
        age = w.lookback_hours,
        strikes = list(&|r| format!("{:?}", r.strike)),
        expiries = list(&|r| r.expiry_ms.to_string()),
        sell_at = sell_at.join(", "),
    );
    toml::from_str(&text).map_err(|e| StudyError::Study(format!("the profile does not read: {e}")))
}

/// The store the study hands its backtest: the context's read verbs, nothing else. A backtest of this
/// profile reads the underlying's bars and the chain; every other read is refused, never answered empty.
struct ContextStore(StudyContext);

fn refused(verb: &'static str) -> HistoryStoreError {
    HistoryStoreError::Query(format!(
        "{verb}: the covered call's store serves bars and the chain only"
    ))
}

impl HistoryStore for ContextStore {
    fn load_bars(
        &self,
        venue: &str,
        symbol: &str,
        interval: &str,
        range: TimestampRange,
    ) -> Result<Vec<Bar>, HistoryStoreError> {
        self.0.bars(venue, symbol, interval, range)
    }

    fn scan_quotes(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickQuote>, HistoryStoreError> {
        self.0.quotes(venue, symbol, range)
    }

    fn scan_trades(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<TickTrade>, HistoryStoreError> {
        self.0.trades(venue, symbol, range)
    }

    fn scan_book_updates(
        &self,
        venue: &str,
        symbol: &str,
        range: TimestampRange,
    ) -> Result<Vec<L2BookUpdate>, HistoryStoreError> {
        self.0.book_updates(venue, symbol, range)
    }

    fn scan_chain(
        &self,
        venue: &str,
        underlying: &str,
        range: TimestampRange,
    ) -> Result<Vec<ChainRow>, HistoryStoreError> {
        self.0.chain(venue, underlying, range)
    }

    vike_data::hist_store_stubs!(refuse(refused): writes, scan_symbol_properties, scan_equity,
        scan_exec_fills, scan_exec_orders);
}
