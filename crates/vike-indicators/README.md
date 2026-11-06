# vike-indicators

Technical-analysis indicators and candlestick patterns for Rust: 211 of them, streaming or over a whole series, giving the same numbers on both paths and on Windows and Linux.

**Two ways to use it, and the second one is much faster:**

| | 1. With bars | 2. With plain numbers |
|---|---|---|
| You hand over | `Bar`s (`vike_marketdata::Bar`) | `f64` columns or one `f64` at a time |
| Indicators | all 211 | 210 (all but `ichimoku`) |
| Speed | the reference | **3 to 6 times faster over a long series**, because a `Bar` is about 120 bytes and a number 8; the same speed streaming one value |
| In code | `rsi.next(&bar)` for one bar, `indicator.vectorize(&bars)` for a slice | `rsi.next_value(price)` for one number, `series::rsi(&close, 14.0)?` for a slice |

Both run the same code, so they give exactly the same bits. [See both in code](#two-ways-to-use-it) · [benchmarks against the other Rust crates](#benchmarks).

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

## Two ways to use it

**1. With bars** (`vike_marketdata::Bar`), for all 211 indicators. Feed a bar at a time, or hand over a slice of bars and get every output line back by name:

```rust
use vike_indicators::indicators::Rsi;
use vike_marketdata::Bar;

// `bars: Vec<Bar>`
let mut rsi = Rsi::new(14.0)?;                       // typed; a bad parameter is an error
for bar in &bars {
    let value = rsi.next(bar);                        // streaming: one bar in, one value out
}
let lines = vike_indicators::make_with("rsi", &[14.0]) // by name, chosen at run time
    .expect("known indicator")
    .vectorize(&bars);                                // whole series: one Vec<f64> per output line
```

**2. With plain numbers** (`f64`), for 210 of them, the faster way. No `Bar` in the call: columns in, columns out, or one number in, one number out:

```rust
use vike_indicators::{indicators::Rsi, series};

// `close: Vec<f64>`
let whole = series::rsi(&close, 14.0)?;               // whole series from a slice
let mut rsi = Rsi::new(14.0)?;
let value = rsi.next_value(close[0]);                 // streaming: one number in, one value out
```

Both ways run the same code, so a stream, a batch and a slice call give exactly the same bits. Choose the first when your data already is `Bar`s, when you pick indicators by name at run time (a settings dialog), or for `ichimoku`; choose the second when you have price columns, want the most speed, or want no bar type in your code. More below: [a longer example](#example) and [the plain-number door](#without-bar-plain-numbers-the-fast-door).

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

Everything above takes `vike_marketdata::Bar`. If your data is already a column of numbers, you do not need `Bar` or `vike-marketdata` at all: 210 of the 211 indicators (every one except `ichimoku`, which reads future bars) have a second, faster door that takes `f64` directly.

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
- **Which columns a call takes follows from what the indicator reads.** Candlestick patterns take `open, high, low, close` (`series::doji(&open, &high, &low, &close)`, `Doji::next_ohlc`), volume indicators add `volume`, the pair indicators take two aligned series (`series::beta(&primary, &benchmark, 60.0)`, `on_pair_values`), and `vwap` and `anchored_vwap` also take the timestamps. `zigzag` and `williams_fractal` read future bars: they have the slice call only, and their streaming is the best-effort one it always was.
- **The `Bar` door stays available for every indicator**, `ichimoku` included, which has no plain-number door; `vike-marketdata` remains a dependency of the crate.


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
- **Bars in, floats out; or floats in, floats out.** Indicators read `vike_marketdata::Bar` (timestamp in milliseconds, open, high, low, close, volume) and compute in `f64`. 210 of them also take plain `f64` values and slices directly, with no `Bar` (see "Without `Bar`" above, and "Benchmarks" for the speed).

## Benchmarks

Each indicator keeps running sums over its window and rebuilds them from the window now and then, so a streamed value costs a few nanoseconds whatever the period, nothing is allocated per bar, and a whole series is one pass. Reading a plain column also touches 8 bytes per value where a `Bar` is about 120, so on a long series the plain-number door is faster than the `Bar` door: at one million values the `Bar` door measured 3 to 6 times slower for the same indicator. Streaming, where the one bar being fed is already in the cache, the two doors are within about 5% of each other. The tables below are the measured result against the other Rust indicator crates.

### Against other Rust indicator crates

Every indicator measured in one session against the other crates that implement it: `ta` 0.5.0, `yata` 0.7.0, `kand` 0.2.2, `rust_ti` 2.2.0 and `wickra-core` 2.0.0 (with its optional SIMD `batch_fast` where it has one). Shared Linux servers, Rust 1.97, release build, 50,000 values per call; each figure is the minimum of three runs, and each run's figure is the minimum over interleaved rounds of the median pass. **ours** is the faster of the plain-`f64` door and the `Bar` door. **Stream** is one value per call; **series** is `series::*` over a slice, fresh output each call. **rank** is our place among the libraries that compute the same thing (`1/3` is the fastest of three). **†** means every other library measured computes a different definition (another average, a population against a sample deviation, no warm-up, ...), so the figure shown is the best of those and no rank is given; **—** means no other library implements it.

Stream, 211 rows: fastest in 157, second in 15; in 22 only libraries computing a different definition are faster; 17 have no competitor. Series, 215 rows: fastest in 175, second in 10, lower in 2; in 9 only libraries computing a different definition are faster; 19 have no competitor. Every cell where we are not first has its reason in the note column. The four `rvol_*` estimators stay behind `wickra` because the crate keeps the pure-Rust `libm` logarithm so the values are bit-identical on Windows and Linux, where `wickra` uses the platform's.

#### Trend

##### Moving averages

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| alma(20,0.85,6) | 4.43 | wickra 12.3 | 1/2 | 224 | wickra 658 | 1/2 |  |
| dema(20) | 1.16 | wickra 5.54 | 1/2 | 51.0 | wickra-fast 63.1 | 1/4 |  |
| ema(20) | 1.40 | ta 1.30 | 2/6 | 76.0 | yata 108 | 1/5 | stream: 7% behind ta (1.40 against 1.30): a serial one-multiply recurrence, both at the dependency-chain floor |
| evwma(20) | 3.43 | wickra 6.17 | 1/2 | 176 | wickra 319 | 1/2 |  |
| frama(16) | 33.6 | wickra 77.8 † | † | 1681 | wickra 3929 † | † |  |
| hma(20) | 4.14 | wickra 9.35 | 1/2 | 138 | wickra-fast 172 | 1/3 |  |
| kama(10,2,30) | 2.54 | wickra 12.1 | 1/3 | 140 | wickra 663 | 1/2 |  |
| mcginley(14) | 2.90 | wickra 8.66 † | † | 154 | rust_ti 536 | 1/3 |  |
| median_ma(20) | 29.5 | wickra 44.3 | 1/2 | 1469 | wickra 2171 | 1/3 |  |
| sma(20) | 0.80 | kand 1.00 | 1/6 | 45.4 | kand 72.3 | 1/5 |  |
| smma(14) | 1.33 | yata 2.18 | 1/3 | 72.3 | yata 124 | 1/3 |  |
| t3(20,0.7) | 3.06 | wickra 17.9 | 1/2 | 160 | wickra 1079 | 1/3 |  |
| tema(20) | 1.80 | wickra 8.36 | 1/2 | 96.0 | wickra-fast 87.2 | 2/4 | series: wickra's batch_fast (wickra-simd) 87.2 against ours 96.0 |
| trima(20) | 1.45 | kand 2.08 | 1/3 | 74.4 | wickra-fast 54.8 | 2/4 | stream: yata's TRIMA seeds with the first value and has no warm-up, so it is not like-for-like; series: wickra's batch_fast (wickra-simd) 54.8 against ours 74.4 |
| vidya(14,9) | 3.47 | wickra 6.85 | 1/2 | 179 | wickra 369 | 1/2 |  |
| vwma(20) | 1.94 | wickra 6.36 | 1/2 | 96.3 | yata 140 | 1/3 |  |
| wma(20) | 1.45 | wickra 2.70 | 1/2 | 81.5 | wickra 111 | 1/4 |  |
| zlema(20) | 1.75 | wickra 5.33 † | † | 40.6 | wickra 287 | 1/2 |  |

##### Channels, stops and multi-line systems

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| alligator | 3.57 | wickra 6.78 † | † | 194 | wickra 345 † | † |  |
| atr_trailing_stop(14,3) | 4.18 | wickra 4.02 | 2/2 | 209 | wickra 209 | 1/2 | stream: 4% behind wickra (4.18 against 4.02) |
| chandelier(22,3) | 4.56 | wickra 27.2 | 1/2 | 256 | wickra 1373 | 1/2 |  |
| envelopes(20,2.5) | 2.07 | wickra 1.89 † | † | 85.4 | wickra 291 † | † | stream: wickra's MaEnvelope is not the same series |
| gmma | 6.63 | wickra 29.5 | 1/2 | 590 | wickra 1390 | 1/2 |  |
| hilo_activator(3) | 8.14 | wickra 12.9 † | † | 391 | wickra 611 † | † |  |
| midpoint(14) | 3.09 | wickra 16.0 | 1/2 | 120 | ta 488 | 1/3 |  |
| midprice(14) | 2.71 | wickra 17.9 | 1/2 | 121 | ta 505 | 1/3 |  |
| psar | 3.77 | kand 4.39 † | † | 188 | wickra 404 | 1/3 |  |
| supertrend(10,3) | 3.86 | wickra 17.8 | 1/2 | 206 | wickra 899 | 1/2 |  |

#### Momentum

##### Oscillators and rate of change

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| ac | 3.20 | wickra 5.97 | 1/2 | 237 | wickra 461 | 1/2 |  |
| ao(5,34) | 2.96 | wickra 5.61 | 1/3 | 151 | wickra 294 | 1/3 |  |
| apo(12,26) | 0.93 | wickra 5.08 | 1/2 | 41.8 | wickra 260 | 1/2 |  |
| asi(1) | 3.30 | — | — | 176 | — | — |  |
| bop | 0.52 | wickra 1.04 | 1/3 | 26.3 | wickra 35.0 | 1/3 |  |
| cci(20) | 9.09 | ta 14.8 | 1/4 | 455 | kand 680 | 1/6 |  |
| chande_kroll_stop(10,1,9) | 7.07 | wickra 33.1 † | † | 366 | wickra 1704 | 1/3 |  |
| cmo(14) | 3.15 | wickra 5.72 | 1/2 | 163 | wickra 286 | 1/4 |  |
| connors_rsi(3,2,100) | 27.4 | wickra 38.9 | 1/2 | 1296 | wickra 1874 | 1/2 |  |
| coppock(10,14,11) | 4.35 | wickra 10.4 | 1/3 | 189 | wickra 520 | 1/3 |  |
| dpo(20) | 1.19 | yata 4.52 | 1/3 | 55.1 | wickra 241 | 1/3 |  |
| elder_ray(13) | 1.36 | wickra 2.72 | 1/2 | 77.3 | wickra 186 | 1/2 |  |
| fisher(9) | 17.5 | wickra 30.5 † | † | 854 | wickra 1513 | 1/3 |  |
| kst(defaults) | 14.3 | yata 21.6 | 1/3 | 603 | yata 1368 | 1/3 |  |
| macd(12,26,9) | 1.50 | ta 1.50 | 1/6 | 93.4 | wickra 177 | 1/6 |  |
| mom(10) | 0.49 | kand 0.76 | 1/4 | 10.6 | kand 11.7 | 1/4 |  |
| ppo(12,26) | 1.63 | wickra 5.31 | 1/2 | 63.4 | ta 83.0 | 1/3 | stream: within 2.6% of ta, whose PPO is another shape (adds a signal EMA(9), seeds on the first input) so it is excluded; the like-for-like competitor (wickra 5.3) is 3x slower |
| qstick(8) | 1.83 | wickra 3.71 | 1/2 | 74.7 | wickra 191 | 1/2 |  |
| roc | 0.99 | kand 1.02 | 1/4 | 26.3 | kand 29.9 | 1/4 |  |
| roc1 | — | — | — | 26.5 | rust_ti 60.5 | 1/2 |  |
| rocp(10) | 1.04 | yata 1.03 | 2/5 | 26.6 | kand 29.6 | 1/5 | stream: tie at the ~1 ns floor (one ring read and one divide): yata 1.03 against ours 1.04 (1%) |
| rocr(10) | 1.04 | kand 1.09 | 1/3 | 26.6 | kand 29.5 | 1/3 |  |
| rocr100(10) | 1.07 | kand 1.06 | 2/3 | 26.5 | kand 30.0 | 1/3 | stream: tie at the ~1 ns floor: kand 1.06 against ours 1.07 (0.9%) |
| rsi(14) | 1.70 | ta 4.20 | 1/6 | 93.9 | wickra-fast 80.7 | 2/6 | series: wickra's batch_fast (wickra-simd) is a SIMD path at 80.7; ours is the scalar Wilder recursion at 93.9; wickra's non-SIMD batch_nan is 117.4 |
| smi ergodic(20,5,5) | 2.57 | yata 17.1 † | † | 144 | yata 860 | 1/2 |  |
| stochastic(14,3,3) | 9.41 | yata 24.9 | 1/2 | 511 | ta 579 | 1/4 |  |
| stochf(14,3) | 5.53 | wickra 35.7 | 1/2 | 292 | wickra 1796 | 1/2 |  |
| stochrsi | 13.3 | wickra 23.7 † | † | 634 | wickra 1217 † | † |  |
| trix(18) | 1.80 | wickra 8.77 | 1/3 | 103 | kand 345 | 1/4 |  |
| tsi(25,13) | 1.75 | wickra 13.0 | 1/3 | 90.5 | yata 440 | 1/3 |  |
| ultosc(7,14,28) | 7.61 | wickra 9.09 | 1/2 | 377 | wickra 423 | 1/2 |  |
| williams %r(14) | 3.64 | wickra 22.4 | 1/2 | 175 | kand 699 | 1/4 |  |

##### Directional movement and trend strength

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| adx | 5.84 | kand 8.10 | 1/3 | 306 | kand 727 | 1/5 |  |
| adxr(14) | 6.03 | kand 8.89 † | † | 314 | wickra 772 † | † |  |
| aroon(14) | 3.46 | wickra 24.8 | 1/2 | 189 | kand 982 | 1/5 | stream: kand's aroon_inc is not the same series, so it is not like-for-like |
| aroonosc(14) | 2.98 | wickra 22.4 | 1/2 | 151 | kand 931 | 1/4 | stream: kand's aroonosc_inc is not the same series, so it is not like-for-like |
| dx(14) | 3.41 | kand 5.63 | 1/3 | 176 | wickra 546 | 1/3 |  |
| minus_di(14) | 2.76 | kand 4.45 | 1/3 | 145 | wickra 231 | 1/3 |  |
| minus_dm(14) | 1.35 | wickra 4.24 | 1/3 | 72.9 | kand 216 | 1/3 |  |
| plus_di(14) | 2.89 | wickra 4.48 | 1/3 | 151 | wickra 244 | 1/3 |  |
| plus_dm(14) | 1.35 | kand 4.23 | 1/3 | 73.3 | kand 216 | 1/3 |  |
| relative_vigor(10) | 4.85 | yata 28.6 | 1/2 | 221 | yata 1752 | 1/3 |  |
| rwi(14) | 6.48 | wickra 88.7 | 1/2 | 314 | wickra 4446 | 1/2 |  |
| vortex(14) | 4.61 | wickra 6.90 | 1/2 | 256 | wickra 390 | 1/2 |  |

#### Volatility

##### Range, bands and channels

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| acceleration_bands(20,4) | 5.29 | wickra 14.2 | 1/2 | 256 | wickra 720 | 1/2 |  |
| atr(14) | 1.60 | ta 1.50 | 2/6 | 86.4 | wickra-fast 60.5 | 3/5 | stream: 6% behind ta (1.50 against 1.60): ATR is a serial Wilder recursion, both near the dependency-chain floor; series: wickra-fast SIMD 60.5, and wickra's batch_nan_into 85.4 is 1% ahead of ours (86.4) |
| bb(20,2) | 2.90 | ta 4.50 | 1/6 | 180 | wickra-fast 208 | 1/6 |  |
| bbands_pctb(20,2) | 3.86 | wickra 10.6 | 1/2 | 202 | wickra 535 | 1/2 |  |
| bbands_width(20,2) | 3.76 | wickra 10.3 | 1/2 | 195 | wickra 523 | 1/2 |  |
| chaikin_volatility(10,10) | 2.84 | wickra 5.12 | 1/2 | 77.6 | wickra 240 | 1/2 |  |
| chop(14) | 19.5 | wickra 43.1 | 1/2 | 914 | wickra 2157 | 1/2 |  |
| donchian(20) | 3.54 | yata 12.7 | 1/3 | 182 | yata 697 | 1/4 |  |
| donchian_width(20) | 3.08 | ta 10.8 | 1/3 | 159 | ta 561 | 1/3 |  |
| high_low_52w(252) | 2.24 | ta 5.49 | 1/4 | 114 | ta 351 | 1/5 |  |
| keltner(20,10,2) | 2.39 | wickra 6.41 | 1/3 | 128 | ta 128 | 1/4 |  |
| linreg_channel(100,2) | 5.77 | wickra 274 | 1/2 | 266 | wickra 13842 | 1/2 |  |
| mass(25,9) | 3.81 | wickra 6.78 | 1/2 | 200 | wickra 349 | 1/2 |  |
| natr(14) | 2.05 | wickra 3.36 | 1/3 | 110 | wickra 172 | 1/3 |  |
| starc(6,15,2) | 3.95 | wickra 7.10 | 1/2 | 649 | wickra 816 | 1/2 |  |
| stddev(20) | 2.19 | kand 6.33 | 1/3 | 109 | wickra 363 | 1/4 |  |
| true_range | 0.44 | yata 0.59 | 1/6 | 20.2 | rust_ti 33.9 | 1/4 |  |
| ttm_squeeze(20,2,1.5) | 17.0 | wickra 70.0 | 1/2 | 781 | wickra 3489 | 1/2 |  |
| ulcer(14) | 7.02 | wickra 16.7 † | † | 354 | wickra 842 † | † |  |

##### Realized volatility

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| hvol(20,365) | 14.2 | wickra 18.8 † | † | 684 | wickra 958 † | † |  |
| relative_volatility(14) | 5.66 | wickra 18.8 † | † | 263 | wickra 911 | 1/2 |  |
| rvol_close_to_close(20) | 16.7 | wickra 19.2 | 1/2 | 692 | wickra 971 | 1/2 |  |
| rvol_ewma(0.94) | 13.2 | wickra 8.24 | 2/2 | 522 | wickra 414 | 2/2 | stream and series: keeps libm::log; wickra uses the platform ln. |
| rvol_garman_klass(20) | 24.9 | wickra 17.4 | 2/2 | 1121 | wickra 873 | 2/2 | stream and series: keeps libm::log; wickra uses the platform ln. |
| rvol_parkinson(20) | 15.7 | wickra 11.5 | 2/2 | 623 | wickra 580 | 2/2 | stream and series: keeps libm::log; wickra uses the platform ln. |
| rvol_rogers_satchell(20) | 33.7 | wickra 26.4 | 2/2 | 1423 | wickra 1327 | 2/2 | stream and series: keeps libm::log; wickra uses the platform ln. |
| rvol_yang_zhang(20) | 39.0 | wickra 45.0 | 1/2 | 1861 | wickra 2256 | 1/2 |  |

#### Volume

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| ad | 1.05 | wickra 1.05 | 2/4 | 58.6 | kand 57.1 | 3/5 | stream: tie, 0.1%: wickra 1.046 against ours 1.047; series: three libraries within 3%: kand 57.06, wickra 58.24, ours 58.61 (a one-pass memory-bound loop, 1.2 ns/value) |
| adosc(3,10) | 1.63 | kand 4.58 † | † | 93.7 | kand 762 † | † |  |
| anchored_vwap(0) | 1.10 | wickra 2.07 | 1/3 | 60.0 | wickra 109 | 1/3 |  |
| cmf(20) | 3.34 | yata 8.50 | 1/3 | 182 | wickra 553 | 1/3 |  |
| efi(13) | 1.38 | wickra 2.75 | 1/2 | 75.1 | wickra 145 | 1/2 |  |
| eom(14) | 3.12 | wickra 16.4 | 1/2 | 162 | wickra 828 | 1/2 |  |
| kvo(34,55,13) | 1.88 | wickra 10.8 † | † | 135 | wickra 533 † | † |  |
| mfi(14) | 3.32 | ta 11.2 | 1/3 | 175 | ta 594 | 1/6 |  |
| net_volume | 0.33 | — | — | 23.5 | — | — |  |
| nvi | 1.17 | wickra 2.38 | 1/2 | 65.0 | wickra 123 | 1/2 |  |
| obv | 0.67 | wickra 3.97 | 1/3 | 39.6 | kand 202 | 1/4 |  |
| pvi | 1.17 | wickra 2.35 | 1/2 | 65.1 | wickra 124 | 1/2 |  |
| pvt | 1.04 | wickra 1.08 | 1/2 | 58.9 | wickra 59.8 | 1/2 |  |
| volume_osc(5,10) | 1.53 | wickra 6.41 † | † | 82.9 | wickra 313 † | † |  |
| vwap (UTC day) | 1.62 | wickra 11.8 | 1/2 | 87.1 | wickra 594 | 1/2 |  |
| williams_ad | 0.93 | wickra 4.38 | 1/2 | 52.8 | wickra 242 | 1/2 |  |

#### Statistics

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| autocorr(30,1) | 6.34 | wickra 71.7 † | † | 345 | wickra 3562 | 1/2 |  |
| hurst(100) | 361 | wickra 670 † | † | 18018 | wickra 34259 † | † |  |
| kurtosis(20) | 8.43 | wickra 10.0 † | † | 435 | wickra 518 † | † |  |
| linearreg(14) | 3.53 | wickra 18.6 | 1/2 | 183 | yata 322 | 1/3 |  |
| linearreg_angle(14) | 8.37 | wickra 27.4 | 1/2 | 440 | wickra 1394 | 1/2 |  |
| linearreg_intercept(14) | 3.42 | wickra 17.7 | 1/2 | 180 | wickra 910 | 1/2 |  |
| linearreg_slope(14) | 3.30 | wickra 13.8 | 1/2 | 174 | wickra 703 | 1/2 |  |
| mad(20) | 7.82 | ta 10.2 | 1/2 | 399 | yata 421 | 1/3 |  |
| r_squared(14) | 3.42 | wickra 19.2 | 1/2 | 177 | wickra 964 | 1/2 |  |
| rank_correlation(14) | 32.1 | wickra 222 † | † | 1615 | wickra 11167 | 1/2 |  |
| skew(20) | 6.63 | wickra 9.83 † | † | 325 | wickra 499 † | † |  |
| std_error(20) | 4.16 | wickra 60.0 † | † | 215 | wickra 3015 | 1/2 |  |
| std_error_bands(20,2) | 6.16 | wickra 60.5 † | † | 295 | wickra 3164 | 1/2 |  |
| tsf(14) | 3.49 | wickra 18.4 | 1/2 | 187 | wickra 935 | 1/2 |  |
| var(20) | 1.96 | kand 6.36 | 1/3 | 113 | kand 338 | 1/3 |  |
| zscore(20) | 3.69 | wickra 8.19 | 1/2 | 197 | wickra 447 | 1/2 |  |

#### Candlestick patterns

##### One-bar patterns

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| belt_hold | 5.31 | wickra 6.19 † | † | 157 | wickra 316 † | † |  |
| closing_marubozu | 2.76 | wickra 2.36 † | † | 77.9 | wickra 123 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| doji | 2.46 | wickra 1.41 † | † | 77.2 | wickra 78.2 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| dragonfly_doji | 5.05 | wickra 1.62 † | † | 115 | wickra 80.0 † | † | stream and series: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| gravestone_doji | 4.99 | wickra 1.75 † | † | 112 | wickra 82.7 † | † | stream and series: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| hammer | 2.24 | wickra 2.04 † | † | 60.1 | wickra 100 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| hanging_man | 2.25 | wickra 1.90 † | † | 59.9 | wickra 98.6 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| high_wave | 2.21 | wickra 1.69 † | † | 58.7 | wickra 84.8 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| inverted_hammer | 2.26 | wickra 1.95 † | † | 59.7 | wickra 98.6 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| long_line | 3.24 | wickra 15.8 † | † | 83.6 | wickra 681 † | † |  |
| longlegged_doji | 4.70 | wickra 1.77 † | † | 112 | wickra 83.8 † | † | stream and series: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| marubozu | 2.82 | wickra 2.27 † | † | 96.5 | wickra 122 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| opening_marubozu | 2.77 | wickra 2.27 † | † | 77.5 | wickra 120 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| rickshaw_man | 5.45 | — | — | 130 | — | — |  |
| shooting_star | 2.25 | wickra 2.03 † | † | 59.6 | wickra 102 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| short_line | 2.55 | wickra 15.8 † | † | 78.5 | wickra 677 † | † |  |
| spinning_top | 4.92 | wickra 4.28 † | † | 116 | wickra 228 † | † | stream: wickra compares the body with the bar's own range, a different rule (speed comparison only); ours compares with the mean of the last ten bodies  |
| takuri | 4.85 | — | — | 105 | — | — |  |

##### Two-bar patterns

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| counterattack | 4.14 | wickra 1.65 † | † | 186 | wickra 108 † | † | stream and series: wickra's rule is a different, simpler test (speed comparison only); ours applies the library's own conditions exactly, including the mean of the last ten bodies where the pattern reads it |
| dark_cloud_cover | 1.35 | wickra 1.62 † | † | 35.5 | wickra 274 † | † |  |
| doji_star | 3.61 | wickra 5.31 † | † | 175 | wickra 287 † | † |  |
| engulfing | 1.84 | wickra 6.33 † | † | 47.6 | wickra 322 † | † |  |
| gap_side_side_white | 3.20 | — | — | 163 | — | — |  |
| harami | 2.20 | wickra 6.53 † | † | 66.1 | wickra 322 † | † |  |
| harami_cross | 3.88 | wickra 7.51 † | † | 195 | wickra 401 † | † |  |
| homing_pigeon | 1.56 | wickra 3.51 † | † | 45.7 | wickra 205 † | † |  |
| in_neck | 1.51 | wickra 6.11 † | † | 44.4 | wickra 283 † | † |  |
| kicking | 3.14 | wickra 1.19 † | † | 123 | wickra 58.0 † | † | stream and series: wickra's rule is a different, simpler test (speed comparison only); ours applies the library's own conditions exactly, including the mean of the last ten bodies where the pattern reads it |
| kicking_by_length | 3.63 | wickra 1.19 † | † | 166 | wickra 59.9 † | † | stream and series: wickra's rule is a different, simpler test (speed comparison only); ours applies the library's own conditions exactly, including the mean of the last ten bodies where the pattern reads it |
| matching_low | 2.44 | wickra 4.93 † | † | 125 | wickra 255 † | † |  |
| meeting_lines | 4.14 | — | — | 187 | — | — |  |
| on_neck | 1.42 | wickra 5.92 † | † | 42.7 | wickra 281 † | † |  |
| piercing | 1.31 | wickra 1.62 † | † | 35.4 | wickra 276 † | † |  |
| separating_lines | 3.30 | wickra 1.59 † | † | 165 | wickra 86.3 † | † | stream and series: wickra's rule is a different, simpler test (speed comparison only); ours applies the library's own conditions exactly, including the mean of the last ten bodies where the pattern reads it |
| tasuki_gap | 2.34 | wickra 3.85 † | † | 71.8 | wickra 282 † | † |  |
| thrusting | 1.44 | wickra 6.91 † | † | 44.9 | wickra 348 † | † |  |

##### Three- to five-bar patterns

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| abandoned_baby | 3.24 | — | — | 147 | — | — |  |
| advance_block | 5.48 | — | — | 252 | — | — |  |
| breakaway | 4.87 | — | — | 251 | — | — |  |
| concealing_baby_swallow | 5.39 | — | — | 182 | — | — |  |
| evening_doji_star | 1.94 | wickra 3.60 † | † | 96.9 | wickra 333 † | † |  |
| evening_star | 1.99 | wickra 5.81 † | † | 97.1 | wickra 332 † | † |  |
| hikkake | 2.28 | wickra 5.07 † | † | 54.1 | wickra 317 † | † |  |
| hikkake_mod | 2.66 | wickra 4.62 † | † | 85.6 | wickra 348 † | † |  |
| identical_three_crows | 5.93 | wickra 4.50 † | † | 299 | wickra 293 † | † | stream and series: wickra defines the pattern differently (no average-body context; speed comparison only); ours applies the library's own conditions exactly over the last 3 to 5 bars plus the mean of the last ten bodies |
| ladder_bottom | 3.96 | — | — | 188 | — | — |  |
| mat_hold | 2.43 | — | — | 108 | — | — |  |
| morning_doji_star | 2.08 | wickra 3.86 † | † | 96.6 | wickra 331 † | † |  |
| morning_star | 2.01 | wickra 6.01 † | † | 96.8 | wickra 335 † | † |  |
| rise_fall_three_methods | 4.33 | wickra 5.20 † | † | 223 | wickra 405 † | † |  |
| stalled_pattern | 5.36 | — | — | 270 | — | — |  |
| stick_sandwich | 3.19 | — | — | 161 | — | — |  |
| three_black_crows | 4.76 | wickra 5.87 † | † | 233 | wickra 317 † | † |  |
| three_inside | 1.80 | wickra 6.41 † | † | 85.7 | wickra 423 † | † |  |
| three_line_strike | 4.30 | wickra 5.04 † | † | 204 | wickra 374 † | † |  |
| three_outside | 1.90 | wickra 7.14 † | † | 90.3 | wickra 416 † | † |  |
| three_stars_in_south | 2.24 | wickra 3.16 † | † | 75.8 | wickra 341 † | † |  |
| three_white_soldiers | 4.95 | wickra 6.02 † | † | 236 | wickra 320 † | † |  |
| tristar | 8.55 | wickra 2.02 † | † | 410 | wickra 320 † | † | stream and series: wickra defines the pattern differently (no average-body context; speed comparison only); ours applies the library's own conditions exactly over the last 3 to 5 bars plus the mean of the last ten bodies |
| two_crows | 1.88 | wickra 3.91 † | † | 97.3 | wickra 338 † | † |  |
| unique_three_river | 3.68 | wickra 5.84 † | † | 187 | wickra 458 † | † |  |
| upside_gap_two_crows | 2.57 | wickra 4.04 † | † | 126 | wickra 326 † | † |  |
| xside_gap_three_methods | 3.03 | wickra 0.94 † | † | 79.7 | wickra 260 † | † | stream: wickra defines the pattern differently (no average-body context; speed comparison only); ours applies the library's own conditions exactly over the last 3 to 5 bars plus the mean of the last ten bodies |

#### Price

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| avgprice | 1.23 | wickra 0.76 | 2/2 | 16.1 | wickra 34.7 | 1/2 | stream: one-expression price transform at the ~1 ns/value floor; wickra reads a prebuilt Candle, ours is a call with 2-4 plain f64 arguments; the gap is 0.1-0.5 ns; cause not traced |
| heikin_ashi | 1.39 | wickra 1.41 | 1/3 | 130 | kand 177 | 1/3 |  |
| medprice | 0.86 | wickra 0.73 | 2/3 | 10.4 | kand 14.4 | 1/3 | stream: one-expression price transform at the ~1 ns/value floor; wickra reads a prebuilt Candle, ours is a call with 2-4 plain f64 arguments; the gap is 0.1-0.5 ns; cause not traced |
| typprice | 1.16 | wickra 1.05 | 2/3 | 25.3 | kand 29.7 | 1/3 | stream: one-expression price transform at the ~1 ns/value floor; wickra reads a prebuilt Candle, ours is a call with 2-4 plain f64 arguments; the gap is 0.1-0.5 ns; cause not traced |
| wclprice | 1.19 | wickra 0.70 | 2/3 | 13.7 | wickra 31.2 | 1/3 | stream: one-expression price transform at the ~1 ns/value floor; wickra reads a prebuilt Candle, ours is a call with 2-4 plain f64 arguments; the gap is 0.1-0.5 ns; cause not traced |

#### Structure

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| pivot_points | 1.34 | wickra 1.04 † | † | 101 | wickra 150 | 1/2 | stream: wickra's ClassicPivots reads the CURRENT bar (same arithmetic, other definition), so it is not like-for-like |
| volume_profile_poc(20, 24) | 72.7 | wickra 518 † | † | 3732 | wickra 24462 | 1/2 |  |
| volume_profile_poc(200, 100) | 274 | wickra 3370 † | † | 15610 | wickra 186966 | 1/2 |  |
| volume_profile_poc(50, 24) | 118 | wickra 928 † | † | 6249 | wickra 46350 | 1/2 |  |
| williams_fractal(2) | — | — | — | 328 | wickra 309 | 2/2 | series: wickra's WilliamsFractals is fixed at n = 2 (a batch of update); ours takes n at run time; at n = 5 wickra has no row and the cell has no competitor |
| williams_fractal(5) | — | — | — | 283 | — | — |  |
| zigzag(5.0) | — | — | — | 89.8 | — | — |  |

#### Pair indicators

| Indicator | Stream ns: ours | best other | rank | Series µs: ours | best other | rank | Note |
|---|--:|---|:-:|--:|---|:-:|---|
| beta | 5.47 | wickra 17.7 | 1/2 | 252 | wickra 875 | 1/2 |  |
| correl | 5.34 | wickra 14.6 | 1/2 | 242 | wickra-fast 241 | 2/4 | series: tie (0.6%) with wickra's SIMD batch_pairs_fast_into (241.0 against 242.4); wickra's non-SIMD batch is 455 |
| correl_log | 26.8 | wickra 13.7 | 2/2 | 945 | wickra 455 | 2/2 | stream and series: wickra is handed the log returns PRECOMPUTED (speed only); ours takes two `libm::log` calls per value (libm stays so the values are bit-identical on Windows and Linux) |
| half_life | 6.39 | wickra 54.8 | 1/2 | 307 | wickra 2582 | 1/2 |  |
| kalman_beta | 10.1 | wickra 12.7 | 1/2 | 518 | wickra 637 | 1/2 |  |
| ratio | 1.22 | — | — | 55.7 | — | — |  |
| spread | 0.83 | — | — | 14.9 | — | — |  |
| spread log | 12.1 | — | — | 545 | — | — |  |
| spread_zscore | 4.05 | wickra 8.73 | 1/2 | 181 | wickra 422 | 1/2 |  |

## Stability

This is a `0.x` crate: the API may change between minor versions. Pin the version you tested against if you depend on exact numbers.

## License

FSL-1.1-ALv2, the [Functional Source License](https://fsl.software/) with Apache-2.0 as the future license. It is source-available: you may use, modify and redistribute the code for any purpose except a competing use, as the license defines it. Each version converts to Apache-2.0 on the second anniversary of the date it was made available. Read the license for the exact terms; this summary is not legal advice.

## Disclaimer

This is an indicator toolkit. It computes numbers from price data and gives no financial advice, and nothing here is a recommendation to buy or sell anything. Trading involves risk of loss.
