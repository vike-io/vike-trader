# vike-marketdata

The market-data vocabulary: the plain value types a market feed produces — candles, ticks, order books,
derivatives feed values, option chains — and the small arithmetic that turns one into another.
Pure Rust, one dependency (`serde`), no `unsafe`.

It is the input type of [`vike-indicators`](https://crates.io/crates/vike-indicators) (every indicator
reads a `Bar`), and it works on its own wherever you need a shared, serializable shape for market data.

```toml
[dependencies]
vike-marketdata = "0.1"
```

## What is in it

| Module | What it holds |
|---|---|
| `Bar` | One OHLCV candle: `ts` (epoch ms, UTC), open/high/low/close/volume, plus optional funding, bid, ask and symbol. `Bar::ohlcv(..)` builds the plain case. |
| `QuoteTick`, `TradeTick` | One best-bid/ask update and one trade, with an optional receive timestamp for latency replay. |
| `BarConsolidator`, `consolidate_trades`, `consolidate_quotes` | Ticks to bars, and bars to coarser bars (1m to 1h). A window closes only when a strictly later one starts, so a jittery live feed can never step backwards in time. |
| `L2Book`, `BookUpdate` | A level-2 order book and the delta payload that updates it, with sequence-gap handling and walk-the-book price estimators. |
| `feed` values | Mark price, index price, funding rate, open interest, liquidation, volatility-index tick. |
| Option types | `OptionKind`, `OptionTicker` and the option-chain snapshot types (`OptionChain`, `Expiry`, `StrikeRow`, `OptionQuote`). |
| `aggressor` | Which side of a trade crossed the spread (`Side`, `classify`, `signed`). |
| `interval` | Bar intervals: `interval_ms("5m")`, `KLINE_MENU`. |

## Example

```rust
use vike_marketdata::{consolidate_trades, Bar, TradeTick};

// Three trades, folded into 60-second candles.
let trade = |ts: i64, price: f64, size: f64| TradeTick {
    ts,
    local_ts: 0,
    price,
    size,
    is_buyer_maker: false,
    symbol: String::new(),
};
let ticks = [trade(1_000, 100.0, 1.0), trade(31_000, 101.0, 2.0), trade(61_000, 99.5, 1.0)];
let bars: Vec<Bar> = consolidate_trades(&ticks, 60_000);

// Or build a candle by hand:
let bar = Bar::ohlcv(0, 100.0, 101.5, 99.0, 100.8, 1250.0);
assert_eq!(bar.close, 100.8);
```

## Compatibility

- The types derive `Serialize` and `Deserialize`. Fields added after the first release carry
  `#[serde(default)]`, so older payloads keep deserializing.
- This is a `0.x` crate: the API can change between minor versions.

## License

FSL-1.1-ALv2 (Functional Source License, converting to Apache-2.0 two years after each release).
See the `license` field in `Cargo.toml` and the license file in the repository.
