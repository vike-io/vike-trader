# vike-indicators

Technical-analysis indicators and candlestick patterns for Rust, with one streaming and one batch path that give the same numbers.

**211 indicators** · 63 candlestick patterns · 8 categories

| Category | Indicators |
|---|---|
| Trend | 29 |
| Momentum | 43 |
| Volatility | 27 |
| Volume | 16 |
| Statistics | 16 |
| Patterns | 63 |
| Price | 5 |
| Structure | 4 |
| Pair (benchmark) | 8 |

The total includes the 63 candlestick patterns and the 8 pair (benchmark) indicators, which take two price series instead of one.

## Install

```toml
[dependencies]
vike-indicators = "0.1"
vike-marketdata = "0.1"   # the `Bar` type lives here
```

## Example

```rust
use vike_indicators::indicators::{Macd, Rsi};
use vike_marketdata::Bar;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // 200 synthetic one-minute bars: Bar::ohlcv(ts_ms, open, high, low, close, volume).
    let bars: Vec<Bar> = (0..200_i64)
        .map(|i| {
            let c = 100.0 + (i as f64 * 0.1).sin() * 5.0;
            Bar::ohlcv(i * 60_000, c, c + 1.0, c - 1.0, c, 1_000.0)
        })
        .collect();

    // Streaming and typed: one bar in, the value(s) out. NaN until warmed up.
    // A parameter outside its allowed range is an error, not a silent clamp.
    let mut rsi = Rsi::new(14.0)?;
    let mut macd = Macd::new(12.0, 26.0, 9.0)?;
    let (mut r, mut m) = (f64::NAN, [f64::NAN; 3]);
    for bar in &bars {
        r = rsi.next(bar); // f64
        m = macd.next(bar); // [line, signal, hist]
    }
    println!("RSI {r:.2}  MACD {:.3} / {:.3} / {:.3}", m[0], m[1], m[2]);

    // By name, chosen at run time. This path clamps parameters instead of failing.
    let ind = vike_indicators::make_with("rsi", &[14.0]).expect("known indicator");
    let batch = ind.vectorize(&bars); // one Vec<f64> per output line
    assert_eq!(batch[0].last().map(|v| v.to_bits()), Some(r.to_bits())); // batch == streaming

    // Every output line with its name.
    for (name, series) in vike_indicators::compute("macd", &[], &bars).expect("known indicator") {
        println!("{name}: {} values", series.len());
    }
    Ok(())
}
```

## Without `Bar`: plain numbers, the fast door

Everything above takes `vike_marketdata::Bar`. If your data is already a column of numbers, you do not need `Bar` or `vike-marketdata` at all: 102 of the indicators (the whole Trend, Momentum and Volatility categories except `ichimoku`, plus `obv`, `ad`, `mfi`, `cmf`) have a second, faster door that takes `f64` directly.

```rust
use vike_indicators::indicators::{Atr, Rsi};
use vike_indicators::series;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Plain columns: no `Bar`, no `vike-marketdata`.
    let close: Vec<f64> = (0..200).map(|i| 100.0 + (i as f64 * 0.1).sin() * 5.0).collect();
    let high: Vec<f64> = close.iter().map(|c| c + 1.0).collect();
    let low: Vec<f64> = close.iter().map(|c| c - 1.0).collect();

    // A whole series in one call: one value per input, NaN while warming up.
    let rsi = series::rsi(&close, 14.0)?;
    let [macd, signal, hist] = series::macd(&close, 12.0, 26.0, 9.0)?;
    let atr = series::atr(&high, &low, &close, 14.0)?;
    println!("RSI {:.2}  MACD {:.3} / {:.3} / {:.3}", rsi[199], macd[199], signal[199], hist[199]);

    // Streaming: the same indicator types, fed plain numbers (`next_value` for a close,
    // `next_hlc` for high, low and close).
    let mut r = Rsi::new(14.0)?;
    let mut a = Atr::new(14.0)?;
    for i in 0..close.len() {
        let (streamed_rsi, streamed_atr) =
            (r.next_value(close[i]), a.next_hlc(high[i], low[i], close[i]));
        // The stream and the series give exactly the same bits (two NaNs count as equal).
        assert_eq!(streamed_rsi.to_bits(), rsi[i].to_bits());
        assert_eq!(streamed_atr.to_bits(), atr[i].to_bits());
    }
    Ok(())
}
```

- **`series::<name>(columns.., params..)`** computes a whole series from slices. One function per indicator, named like its registry key (`series::sma`, `series::supertrend`, `series::rvol_yang_zhang`, ...); an indicator with several lines returns one `Vec<f64>` per line. It takes the parameters as `f64`, exactly like `Type::new`, and refuses a value the registry would not accept with the same `ParamError`.
- **`next_value`, `next_hl`, `next_hlc`, `next_cv`, `next_hlcv`, `next_ohlc`, `next_oc`** on the concrete type feed one value at a time. Which one exists follows from what the indicator reads (`Rsi` a close, `Atr` high, low and close, `Mfi` high, low, close and volume).
- **Same numbers as the `Bar` door, bit for bit.** `next(&Bar)`, `next_value`/`next_hlc`/..., `vectorize(&[Bar])` and `series::*` all run one and the same step function, so a stream, a batch and a slice call cannot disagree; the test suite checks that for every one of these indicators, and against a textbook reference at three price scales.
- **The `Bar` door is unchanged and stays for every indicator**, including the ones that have no plain-number door (patterns, pairs, `ichimoku`, and the rest of the catalogue); `vike-marketdata` remains a dependency of the crate.

### How much faster

Measured on a shared Linux build server (Rust 1.97, release), minimum of three runs, 50,000 values per call, against the same indicator before this change through `vectorize(&[Bar])`:

| Indicator | `vectorize(&[Bar])` before | `series::*` now | Faster |
|---|---|---|---|
| `dx` | 1,023 µs | 189 µs | 5.4x |
| `chandelier` | 2,597 µs | 265 µs | 9.8x |
| `supertrend` | 858 µs | 220 µs | 3.9x |
| `ttm_squeeze` | 5,540 µs | 686 µs | 8.1x |
| `kst` | 3,040 µs | 564 µs | 5.4x |
| `linreg_channel` | 29,324 µs | 259 µs | 113x |
| `bbands_pctb` | 1,933 µs | 229 µs | 8.4x |
| `ulcer` | 1,833 µs | 409 µs | 4.5x |

Streaming, one value per call, ns per value: `rocp` 630 to 1.1, `bop` 898 to 0.6, `elder_ray` 10,622 to 1.5, `high_low_52w` 216 to 2.2, `chandelier` 51 to 4.9, `tsi` 5.2 to 1.8.

Two separate effects add up. The step functions themselves were rewritten (running sums, no per-call allocation, nothing recomputed from the window), which is most of the table above and benefits the `Bar` door too. On top of that, reading a plain column touches 8 bytes per value where a `Bar` is about 120: on a long series that is memory traffic, and at one million values the `Bar` door measured 3 to 6 times slower than the same indicator on `f64` slices. Streaming, where the one bar being fed is already in the cache, the two doors measured within about 5% of each other, so the gain of the plain-number door is in whole-series calls on long columns.

Against the fastest other Rust indicator crates measured on the same box (`ta`, `yata`, `kand`, `wickra`, `rust_ti`), these indicators are first in most cells in both regimes (a stream and a whole series). The exceptions are listed in the repository's changelog, for example `rvol_ewma`, `rvol_parkinson`, `rvol_garman_klass` and `rvol_rogers_satchell`, which stay behind `wickra` (7 to 55%) because they use the pure-Rust `libm` logarithm to give the same bits on every platform, where `wickra` uses the platform's. The last bits differ from a textbook formula the way every fast implementation does (running sums instead of re-folding the window): each indicator is checked against the textbook form to a relative 1e-9 or better on three price scales, with a few documented exceptions in the changelog.

## Why vike-indicators

- **Streaming equals batch, bit for bit.** Every indicator has an `on_bar` that takes one bar at a time (what a live loop needs) and a `vectorize` that computes a whole series (what a chart or a backtest needs). Folding `on_bar` over a series gives exactly the bits `vectorize` gives, and the test suite checks that for every indicator in the registry. The typed `next` is `on_bar` without the `Vec`.
- **The same numbers on Windows and Linux.** Floating-point `+ - * /` and `sqrt` are exact everywhere, but `ln`, `log10`, `exp` and `atan` are not: each platform's math library may differ in the last bit. Seven indicators were measured hashing differently across platforms; they now call the pure-Rust `libm` crate, and a test pins the values. Develop on one machine, run on another, get the same output.
- **Honest about look-ahead.** Three indicators read future bars (Ichimoku's chikou span, `zigzag`, `williams_fractal`). They are flagged `batch_only` in the registry: `vectorize` is authoritative, and `on_bar` is a causal best effort that the equality test skips. Every other indicator is causal, and the equality test covers it.
- **Warm-up you can ask for.** `lookback(name, params)` and `lookback_full(name, params)` say how many bars an indicator needs before its first value (any line, and every line). Where the depth follows from the parameters it is exact and tested for equality; otherwise it is a conservative lower bound, and `lookback_exact()` tells you which one you have. A few cumulative indicators (for example `vwap` and `obv`) depend on where you started, and `warmup_path_dependent()` flags them.
- **Metadata a chart or a settings screen can use directly.** `registry()` lists every indicator with its display name, category, whether it draws on the price pane or in its own pane, each output line with a render style (line, band, histogram, dots, marker), its reference levels (such as RSI 30/50/70), and for every parameter a default, minimum, maximum and step.
- **A seam between research and trading.** `feature::ColumnFeature` is the same streaming-equals-batch contract over any plain numeric column (a funding rate, an open-interest series), and a streamed feature reuses the batch kernel, so the value a live strategy computes is the one a model was trained on. `frame::Frame` is a validated `(asset, timestamp)` panel (sorted, strictly increasing inside an asset, no duplicates, optionally on a fixed time grid) from which the per-asset row ranges are derived, so a rolling window cannot cross from one asset into the next. The `rolling` blocks apply a feature across a whole panel.
- **Realized volatility.** `realized_vol` offers the RiskMetrics EWMA, close-to-close, Parkinson, Garman-Klass, Rogers-Satchell and Yang-Zhang estimators over bars, as a streaming `RealizedVol` or a batch series. Values are per period; `annualize` and `deannualize` require you to pass the periods per year, and nothing assumes a year length for you.
- **Pair indicators.** Ratio, spread, spread z-score, beta, correlation, log-return correlation, half-life and a Kalman hedge ratio, over two series (`pair_registry()`, `pair_make`).
- **Your own indicators.** `IndicatorMeta::user` describes an indicator you wrote (name, parameters, a factory) in the same metadata shape the built-ins use, so a UI that lists indicators can list yours too. Call it once at startup: each call leaks a small allocation by design.
- **Small and plain.** The only dependencies are `vike-marketdata` (which depends on `serde`) and `libm`. It is pure Rust with no system libraries, and `unsafe` code is forbidden by lint.

## Who it is for

- Teams that want the **backtest and the live bot to run the same indicator code**, with the same bits out.
- People who **develop on Windows and deploy on Linux** (or the reverse) and need identical results.
- **ML feature pipelines** that compute rolling features in research and again, one row at a time, in production.
- **Charting and trading apps** that need an indicator catalogue with names, parameter ranges and render hints.

## What is in it

Names are the registry names, so `make("supertrend")` works as written. The full catalogue, with every parameter and output, is on [docs.rs](https://docs.rs/vike-indicators) and in `registry()`.

| Category | Examples |
|---|---|
| Trend | `sma`, `ema`, `wma`, `dema`, `tema`, `hma`, `supertrend`, `ichimoku`, `kama`, `chandelier` |
| Momentum | `rsi`, `macd`, `stochastic`, `adx`, `cci`, `aroon`, `trix`, `stochrsi`, `dx` |
| Volatility | `atr`, `bollinger`, `keltner`, `donchian`, `natr`, `hvol`, `chop`, `rvol_yang_zhang`, `ttm_squeeze`, `starc` |
| Volume | `obv`, `vwap`, `mfi`, `cmf`, `ad`, `adosc`, `kvo`, `pvt`, `anchored_vwap` |
| Statistics | `linearreg`, `linearreg_slope`, `zscore`, `skew`, `kurtosis`, `var`, `tsf`, `r_squared`, `hurst` |
| Patterns | `doji`, `engulfing`, `hammer`, `harami`, `morning_star`, `evening_star`, `three_white_soldiers`, `abandoned_baby` |
| Price | `avgprice`, `medprice`, `typprice`, `wclprice`, `heikin_ashi` |
| Structure | `pivot_points`, `zigzag`, `williams_fractal`, `volume_profile_poc` |
| Pair | `ratio`, `spread`, `spread_zscore`, `beta`, `correl`, `correl_log`, `half_life`, `kalman_beta` |

## Conventions

- **NaN while warming up.** Positions where an indicator has no value yet are `f64::NAN`, in both the streaming and the batch path. Compare results with `f64::to_bits` (and treat two NaNs as equal) if you need exactness.
- **Candlestick patterns** are signals, not prices: `+100` bullish, `-100` bearish, `0` none.
- **One column per output line.** `on_bar` returns one value per line (MACD has three); `vectorize` returns one series per line, in the order the registry lists them.
- **Typed or by name.** `Rsi::new(14.0)?` refuses an out-of-range or NaN parameter with a `ParamError` that names it; `Rsi::default()` uses the registry defaults. `make_with("rsi", &[14.0])` clamps into range instead, which is what a settings dialog wants.
- **No implicit annualization.** The realized-volatility family returns per-period values, and `annualize` takes a required periods-per-year.
- **Bars in, floats out; or floats in, floats out.** Indicators read `vike_marketdata::Bar` (timestamp in milliseconds, open, high, low, close, volume) and compute in `f64`. 102 of them also take plain `f64` values and slices directly, with no `Bar` (see "Without `Bar`" above).

## Stability

This is a `0.x` crate: the API may change between minor versions. Pin the version you tested against if you depend on exact numbers.

## License

FSL-1.1-ALv2, the [Functional Source License](https://fsl.software/) with Apache-2.0 as the future license. It is source-available: you may use, modify and redistribute the code for any purpose except a competing use, as the license defines it. Each version converts to Apache-2.0 on the second anniversary of the date it was made available. Read the license for the exact terms; this summary is not legal advice.

## Disclaimer

This is an indicator toolkit. It computes numbers from price data and gives no financial advice, and nothing here is a recommendation to buy or sell anything. Trading involves risk of loss.
