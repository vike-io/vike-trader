//! `microstructure` — the trade-tape microstructure study (book chapter 3, W23/W30): how toxic the
//! order flow of one instrument was, and what its information-driven bars look like.
//!
//! A REAL study, committed as a fixture so CI builds it with the runtime builder, runs it in the
//! jail and compares it with a direct run (`tests/pipeline.rs`,
//! `crates/vike-strategy-sandbox/tests/study_child.rs`). It reads the recorded TRADES of
//! `venue`/`symbol` in the study window and computes, with no arithmetic of its own beyond a ratio:
//!
//! * **VPIN** (Easley, López de Prado and O'Hara, "Flow Toxicity and Liquidity in a High-Frequency
//!   World", Review of Financial Studies 25(5), 2012): the tape cut into equal-VOLUME buckets of
//!   `bucket_volume` by `vike_orderflow::VolumeBarBuilder` (a boundary trade is split), each bucket's
//!   order imbalance `|buy - sell| / volume` from the trades' aggressor flags
//!   (`vike_marketdata::aggressor`), and its rolling mean over the last `window` buckets
//!   (`vike_math::rolling::rolling_mean`).
//! * **BVC-VPIN**, the same paper's Bulk Volume Classification, which needs no aggressor flag: a
//!   bucket's buy share is `Φ(ΔP / σ)`, `ΔP` the change of the bucket's close and `σ` the population
//!   standard deviation of the last `window` changes (`vike_math::rolling::rolling_std`), `Φ`
//!   `vike_math::distributions::normal_cdf`; its imbalance is `|2 Φ - 1|`, averaged as above. A window of
//!   equal changes (`σ = 0`) reads `Φ(±∞)` for a move and `NaN` for none.
//! * **The tick rule** (`vike_marketdata::aggressor::tick_rule`): the share of trades whose side it
//!   estimates from prices alone equals the venue's flag, and the VPIN of the tape re-stamped by it.
//! * **Information-driven bars** (López de Prado, "Advances in Financial Machine Learning", 2018,
//!   §2.3): how many dollar bars of `dollar_bucket` (default: `bucket_volume` at the first trade's
//!   price), volume-imbalance bars and volume-runs bars of `bucket_volume` the tape makes. A trade's
//!   dollars are its NOTIONAL, `vike_model::ContractValue::order_notional(size, price)` for the
//!   recipe's `contract_value` (`{ kind = "linear" | "inverse", per_unit = .. }`, default linear 1.0,
//!   what an unlisted symbol reads): an inverse contract's size is already dollars, so its notional
//!   is not `price * size`. A recipe that names none over a symbol whose KEPT row is not linear
//!   (`StudyContext::kept_instrument`: an inverse perpetual or future, a quanto) is REFUSED in words,
//!   never read at linear 1.0: the face is the venue's convention, so the recipe names the contract.
//!
//! Metrics are each series' LAST value; `vpin.tsv` holds every bucket: its end time, VPIN, BVC-VPIN
//! and the tick-rule VPIN.

use vike_marketdata::TickTrade;
use vike_marketdata::aggressor::{TickAggressor, classify, tick_rule};
use vike_math::distributions::normal_cdf;
use vike_math::rolling::{WindowSpec, rolling_mean, rolling_std};
use vike_model::{ContractValue, NotionalKind};
use vike_orderflow::{
    DollarBarBuilder, ImbalanceBarBuilder, OrderflowBar, RunsBarBuilder, VolumeBarBuilder,
};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

fn str_param<'a>(params: &'a toml::Value, key: &str, default: &'a str) -> &'a str {
    params.get(key).and_then(|v| v.as_str()).unwrap_or(default)
}

/// A number a recipe may write as an integer or a float.
fn number_param(params: &toml::Value, key: &str) -> Option<f64> {
    params.get(key).and_then(|v| v.as_float().or_else(|| v.as_integer().map(|i| i as f64)))
}

/// The contract value of a recipe that names none: linear 1.0, what an unlisted symbol reads, unless the
/// symbol's KEPT row (`StudyContext::kept_instrument`) is not linear. Such a row is REFUSED in words: an
/// inverse contract's size is already dollars (a COIN-M tape of 100 USD contracts at 60,000 would read its
/// dollar bars about 600 times too many), and its face is the venue's convention, which the recipe states.
fn unnamed_contract_value(
    ctx: &StudyContext,
    venue: &str,
    symbol: &str,
) -> Result<ContractValue, StudyError> {
    let Some(row) = ctx.kept_instrument(venue, symbol)? else {
        return Ok(ContractValue::default());
    };
    match ContractValue::of(&row.contract, 1.0).map(|v| v.kind) {
        Some(NotionalKind::Linear) => Ok(ContractValue::default()),
        kind => Err(StudyError::Study(format!(
            "{venue} {symbol} is not a linear contract (its kept row reads {kind:?}): name its \
             contract_value in the recipe ({{ kind = \"inverse\", per_unit = <its face> }})"
        ))),
    }
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = str_param(params, "venue", "binance");
    let symbol = str_param(params, "symbol", "BTCUSDT");
    let bucket_volume = number_param(params, "bucket_volume").unwrap_or(1.0);
    let window = params.get("window").and_then(|v| v.as_integer()).unwrap_or(50).max(1) as usize;
    if !bucket_volume.is_finite() || bucket_volume <= 0.0 {
        return Err(StudyError::Study(format!("bucket_volume must be > 0, got {bucket_volume}")));
    }
    let value: ContractValue = match params.get("contract_value") {
        None => unnamed_contract_value(ctx, venue, symbol)?,
        Some(v) => v.clone().try_into().map_err(|e| {
            StudyError::Study(format!(
                "contract_value must be {{ kind = \"linear\" | \"inverse\", per_unit = .. }}: {e}"
            ))
        })?,
    };
    let notional = |t: &TickTrade| value.order_notional(t.size, t.price);

    let trades = ctx.trades(venue, symbol, ctx.window())?;
    let Some(first) = trades.first() else {
        return Err(StudyError::Study(format!(
            "no trades for {venue}/{symbol} in the requested window — the study window is not \
             covered by the store"
        )));
    };
    let dollar_bucket = number_param(params, "dollar_bucket")
        .unwrap_or_else(|| value.order_notional(bucket_volume, first.price));
    if !dollar_bucket.is_finite() || dollar_bucket <= 0.0 {
        return Err(StudyError::Study(format!("dollar_bucket must be > 0, got {dollar_bucket}")));
    }
    let spec = WindowSpec::indicator(window);

    let buckets = VolumeBarBuilder::from_trades(&trades, bucket_volume);
    let flag_vpin = vpin(&buckets, spec);
    let bulk_vpin = bvc_vpin(&buckets, spec);
    let (stamped, agreement) = stamp_by_tick_rule(&trades);
    let tick_rule_vpin = vpin(&VolumeBarBuilder::from_trades(&stamped, bucket_volume), spec);

    let mut out = StudyOutcome::new();
    out.metric("trades", trades.len() as f64)?;
    out.metric("buckets", buckets.len() as f64)?;
    out.metric("vpin", last(&flag_vpin))?;
    out.metric("bvc_vpin", last(&bulk_vpin))?;
    out.metric("tick_rule_agreement", agreement)?;
    out.metric("tick_rule_vpin", last(&tick_rule_vpin))?;
    let dollar_bars = DollarBarBuilder::from_trades(&trades, dollar_bucket, notional).len();
    out.metric("dollar_bars", dollar_bars as f64)?;
    let size = |t: &TickTrade| t.size;
    let imbalance_bars = ImbalanceBarBuilder::from_trades(&trades, bucket_volume, size).len();
    out.metric("imbalance_bars", imbalance_bars as f64)?;
    let runs_bars = RunsBarBuilder::from_trades(&trades, bucket_volume, size).len();
    out.metric("runs_bars", runs_bars as f64)?;

    let mut tsv = String::from("bucket_end_ms\tvpin\tbvc_vpin\ttick_rule_vpin\n");
    for (i, bucket) in buckets.iter().enumerate() {
        let tick = tick_rule_vpin.get(i).copied().unwrap_or(f64::NAN);
        tsv.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            bucket.last_timestamp, flag_vpin[i], bulk_vpin[i], tick
        ));
    }
    out.artifact("vpin.tsv", tsv)?;
    Ok(out)
}

/// VPIN over volume buckets: the rolling mean of each bucket's `|buy - sell| / volume`.
fn vpin(buckets: &[OrderflowBar], spec: WindowSpec) -> Vec<f64> {
    let imbalance: Vec<f64> =
        buckets.iter().map(|b| b.delta.abs() / (b.buy_vol + b.sell_vol)).collect();
    rolling_mean(&imbalance, spec)
}

/// BVC-VPIN over volume buckets: the rolling mean of `|2 Φ(ΔP / σ) - 1|`, `ΔP` the change of the
/// bucket's close (`NaN` for the first bucket) and `σ` the rolling population deviation of `ΔP`.
fn bvc_vpin(buckets: &[OrderflowBar], spec: WindowSpec) -> Vec<f64> {
    let change: Vec<f64> = (0..buckets.len())
        .map(|i| if i == 0 { f64::NAN } else { buckets[i].c - buckets[i - 1].c })
        .collect();
    let sigma = rolling_std(&change, spec);
    let imbalance: Vec<f64> =
        change.iter().zip(&sigma).map(|(dp, s)| (2.0 * normal_cdf(dp / s) - 1.0).abs()).collect();
    rolling_mean(&imbalance, spec)
}

/// The tape with each trade's aggressor flag replaced by the tick rule's estimate, and the share of
/// trades on which the two agree.
fn stamp_by_tick_rule(trades: &[TickTrade]) -> (Vec<TickTrade>, f64) {
    let (mut side, mut previous, mut agree) = (TickAggressor::Buy, f64::NAN, 0usize);
    let stamped: Vec<TickTrade> = trades
        .iter()
        .map(|t| {
            side = tick_rule(t.price, previous, side);
            previous = t.price;
            agree += usize::from(side == classify(t));
            TickTrade { is_buyer_maker: side == TickAggressor::Sell, ..t.clone() }
        })
        .collect();
    (stamped, agree as f64 / trades.len() as f64)
}

fn last(series: &[f64]) -> f64 {
    series.last().copied().unwrap_or(f64::NAN)
}
