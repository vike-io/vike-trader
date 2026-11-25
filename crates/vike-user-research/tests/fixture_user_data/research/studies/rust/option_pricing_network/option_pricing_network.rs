//! `option_pricing_network` — a neural network that prices options, judged against Black–Scholes (the
//! book's options part, "Pricing options with neural networks"). On deribit history: the hourly chains
//! derived from the venue's public trade log (`vike-cli data history fetch deribit:BTC:option-trades`), the
//! underlying's hourly bars beside them.
//!
//! 1. **Samples, point in time.** Each row of the stored chain in the window that has a mark and an
//!    expiry after its own time, at a time the underlying's price is known (the close of its last bar
//!    CLOSED by the row's time, `Bar::closed_at`), is one sample: the features `[K / S (the
//!    moneyness), years to expiry, the snapshot's at-the-money IV of that expiry, 1 for a call or 0 for
//!    a put]` and the target `mark / S`. Every feature is `vike_options::features`': the moneyness
//!    (`moneyness`), the years to expiry (`years_to_expiry_ms`, the crate's 365-day year) and the
//!    at-the-money IV of the row's expiry in the same snapshot (`atm_iv_by_expiry`, the rule of
//!    `vike_options::atm_iv`: the strike nearest the price, the mean of its call's and put's IV),
//!    each row's IV its own, else inverted from its mark (`quote_iv`).
//! 2. **The baseline** is Black–Scholes at that one volatility (`vike_options::black_scholes_price`):
//!    one flat volatility per expiry, no smile. The network sees the same numbers, so what it can add
//!    is the smile and the term structure the flat price misses.
//! 3. **Split by time**: the snapshots of the first `train_fraction` of the window train the network
//!    through the study's learner (the recipe's `[learner]`: a network with `loss = "regression"`), and
//!    the later ones test both pricers.
//!
//! Recipe (no default for anything that names a market):
//!
//! ```toml
//! venue = "deribit"             # the chain's venue
//! chain = "BTC"                 # the kind=chain series
//! underlying_venue = "binance"  # the underlying's bars
//! underlying = "BTCUSDT"
//! interval = "1h"
//! train_fraction = 0.7          # optional
//! learning_rate = 0.003         # optional: the network's step size
//! seed = 0                      # optional
//!
//! [learner]
//! kind = "neural"
//! network = "mlp"
//! hidden = [64, 64]
//! loss = "regression"
//! epochs = 300
//! batch_size = 64
//! ```
//!
//! (On a synthetic smiled chain this network's out-of-sample RMSE was a ninth of the flat price's:
//! `vike-ml-neural-training`'s `tests/options_networks.rs` records the measurement.)
//!
//! Metrics: `rows_train`, `rows_test`, and on the test rows `rmse_network`, `rmse_black_scholes`,
//! `mae_network` and `mae_black_scholes` (prices over the underlying's). Artifacts: `chart_pricing_error.tsv`
//! (each test snapshot's mean absolute error, both pricers) and `model.onnx` when the learner hands its
//! model's text back (a network's).

use vike_data::ChainRow;
use vike_marketdata::{Bar, OptionKind};
use vike_ml_training::{Capture, DEFAULT_POINT, GridPoint, TrainData};
use vike_user_research::{StudyContext, StudyError, StudyOutcome};

/// The features of one sample.
const FEATURES: usize = 4;

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

/// One sample: when it was known, its features, its mark over the price, and the flat-vol price.
struct Sample {
    ts: i64,
    features: [f64; FEATURES],
    target: f64,
    black_scholes: f64,
}

/// The samples of one snapshot (every row sharing `ts`) at the underlying's price `spot`. The
/// features are `vike_options::features`' (the crate's year, its at-the-money rule).
fn snapshot_samples(rows: &[ChainRow], spot: f64, out: &mut Vec<Sample>) {
    let ts = rows[0].ts;
    let live: Vec<&ChainRow> = rows.iter().filter(|row| row.expiry_ms > ts).collect();
    // Each live row's IV (its own, else its mark's), then the at-the-money IV of each expiry.
    let quotes: Vec<(i64, f64, f64)> = live
        .iter()
        .filter_map(|row| {
            let years = vike_options::years_to_expiry_ms(row.expiry_ms, ts);
            vike_options::quote_iv(row.iv, row.mark, spot, row.strike, years, row.kind, 0.0)
                .map(|iv| (row.expiry_ms, row.strike, iv))
        })
        .collect();
    let atm_by_expiry = vike_options::atm_iv_by_expiry(&quotes, spot, ts, 0.0);
    for row in &live {
        let Some(mark) = row.mark.filter(|m| m.is_finite() && *m > 0.0) else { continue };
        let Some(&atm) = atm_by_expiry.get(&row.expiry_ms) else { continue };
        let t = vike_options::years_to_expiry_ms(row.expiry_ms, ts);
        let Some(price) =
            vike_options::black_scholes_price(spot, row.strike, t, atm, row.kind, 0.0)
        else {
            continue;
        };
        let call = if row.kind == OptionKind::Call { 1.0 } else { 0.0 };
        out.push(Sample {
            ts,
            features: [vike_options::moneyness(row.strike, spot), t, atm, call],
            target: mark / spot,
            black_scholes: price / spot,
        });
    }
}

/// The square root of the mean square, and the mean absolute value, of `errors`.
fn rmse_and_mae(errors: &[f64]) -> (f64, f64) {
    let n = errors.len() as f64;
    let squares = vike_math::summation::compensated_sum(errors.iter().map(|e| e * e));
    let absolute = vike_math::summation::compensated_sum(errors.iter().map(|e| e.abs()));
    ((squares / n).sqrt(), absolute / n)
}

pub fn run(ctx: &StudyContext, params: &toml::Value) -> Result<StudyOutcome, StudyError> {
    let venue = text(params, "venue")?;
    let chain = text(params, "chain")?;
    let underlying_venue = text(params, "underlying_venue")?;
    let underlying = text(params, "underlying")?;
    let interval = text(params, "interval")?;
    let train_fraction = number(params, "train_fraction", 0.7);
    let learning_rate = number(params, "learning_rate", DEFAULT_POINT.learning_rate);
    let seed = params.get("seed").and_then(toml::Value::as_integer).unwrap_or(0).max(0) as u64;
    let Some(interval_ms) = vike_marketdata::interval::interval_ms(interval).filter(|ms| *ms > 0)
    else {
        return Err(StudyError::Study(format!("interval {interval:?} is not a bar interval")));
    };
    if !(train_fraction > 0.0 && train_fraction < 1.0) {
        return Err(StudyError::Study(format!("train_fraction {train_fraction} is not in (0, 1)")));
    }
    let learner = ctx.learner().ok_or_else(|| {
        StudyError::NoLearner(format!("a pricing network on {venue}'s {chain} chain"))
    })?;

    let bars: Vec<Bar> = ctx.bars(underlying_venue, underlying, interval, ctx.window())?;
    let chain_rows = ctx.chain(venue, chain, ctx.window())?;
    let mut samples = Vec::new();
    for snapshot in chain_rows.chunk_by(|a, b| a.ts == b.ts) {
        let closed = bars.partition_point(|bar| bar.closed_at(interval_ms, snapshot[0].ts));
        let Some(spot) = closed.checked_sub(1).map(|last| bars[last].close) else { continue };
        snapshot_samples(snapshot, spot, &mut samples);
    }
    let mut times: Vec<i64> = samples.iter().map(|s| s.ts).collect();
    times.dedup();
    let cut = times.get((times.len() as f64 * train_fraction) as usize).copied();
    let (train, test): (Vec<&Sample>, Vec<&Sample>) =
        samples.iter().partition(|s| cut.is_none_or(|cut| s.ts < cut));
    if train.is_empty() || test.is_empty() {
        return Err(StudyError::Study(format!(
            "{} sample(s) over {} snapshot(s): too few to train and to test (fetch the chain and \
             {underlying}'s {interval} bars for the window)",
            samples.len(),
            times.len()
        )));
    }

    let x: Vec<f64> = train.iter().flat_map(|s| s.features).collect();
    let y: Vec<f32> = train.iter().map(|s| s.target as f32).collect();
    let data = TrainData::new(&x, &y, FEATURES)
        .map_err(|e| StudyError::Study(format!("the training matrix is refused: {e}")))?;
    let point = GridPoint { learning_rate, ..DEFAULT_POINT };
    let want = Capture { importance: false, text: true };
    let (model, _, model_text) = learner
        .fit_captured(&data, &point, seed, want)
        .map_err(|e| StudyError::Study(format!("the fit refused: {e}")))?;

    let mut network_errors = Vec::with_capacity(test.len());
    let mut chart = String::from("time\tvalue\tseries\tkind\n");
    for snapshot in test.chunk_by(|a, b| a.ts == b.ts) {
        let mut network_sum = 0.0;
        let mut flat_sum = 0.0;
        for sample in snapshot {
            let network = model.predict_one(&sample.features).map_err(StudyError::Study)?;
            network_errors.push(network - sample.target);
            network_sum += (network - sample.target).abs();
            flat_sum += (sample.black_scholes - sample.target).abs();
        }
        let n = snapshot.len() as f64;
        let ts = snapshot[0].ts;
        chart.push_str(&format!("{ts}\t{}\tnetwork\tline\n", network_sum / n));
        chart.push_str(&format!("{ts}\t{}\tblack_scholes\tline\n", flat_sum / n));
    }
    let flat_errors: Vec<f64> = test.iter().map(|s| s.black_scholes - s.target).collect();
    let (rmse_network, mae_network) = rmse_and_mae(&network_errors);
    let (rmse_black_scholes, mae_black_scholes) = rmse_and_mae(&flat_errors);

    let mut out = StudyOutcome::new();
    out.metric("rows_train", train.len() as f64)?;
    out.metric("rows_test", test.len() as f64)?;
    out.metric("rmse_network", rmse_network)?;
    out.metric("rmse_black_scholes", rmse_black_scholes)?;
    out.metric("mae_network", mae_network)?;
    out.metric("mae_black_scholes", mae_black_scholes)?;
    out.artifact("chart_pricing_error.tsv", chart)?;
    if let Some(model_text) = model_text {
        out.artifact("model.onnx", model_text)?;
    }
    Ok(out)
}
