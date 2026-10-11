//! Multi-timeframe support: synthesize higher timeframes from a fine base stream.
//! Exact port of `core/timeframe.py`. Look-ahead-safe by construction: a coarse bar only
//! becomes visible once its window has fully elapsed (the engine enforces visibility).

use vike_model::Bar;

/// Convert a timeframe string like "30s"/"1m"/"4h"/"1d" to milliseconds. Delegates to the single
/// interval vocabulary in [`vike_model::time::interval_ms`] (which also accepts seconds — a `"30s"`
/// series that loads from the hist store is now a valid backtest timeframe).
pub fn parse_timeframe(tf: &str) -> Result<i64, String> {
    vike_model::time::interval_ms(tf)
        .ok_or_else(|| format!("bad timeframe {tf:?} (expected e.g. '30s', '1m', '4h', '1d')"))
}

/// Aggregate fine `bars` into coarse `Bar`s aligned to epoch windows of `target_ms`.
/// One coarse bar per window, chronological; the final window may be partial.
pub fn resample(bars: &[Bar], target_ms: i64) -> Vec<Bar> {
    let mut out: Vec<Bar> = Vec::new();
    let mut cur_start: Option<i64> = None;
    let (mut o, mut h, mut l, mut c) = (0.0, 0.0, 0.0, 0.0);
    let mut vol = 0.0;
    for b in bars {
        let start = b.ts - b.ts.rem_euclid(target_ms); // Python floor-mod
        if Some(start) != cur_start {
            if let Some(prev) = cur_start {
                out.push(mk(prev, o, h, l, c, vol));
            }
            cur_start = Some(start);
            o = b.open;
            h = b.high;
            l = b.low;
            c = b.close;
            vol = b.volume;
        } else {
            h = h.max(b.high);
            l = l.min(b.low);
            c = b.close;
            vol += b.volume;
        }
    }
    if let Some(prev) = cur_start {
        out.push(mk(prev, o, h, l, c, vol));
    }
    out
}

fn mk(ts: i64, o: f64, h: f64, l: f64, c: f64, vol: f64) -> Bar {
    Bar {
        ts,
        open: o,
        high: h,
        low: l,
        close: c,
        volume: vol,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_timeframe;

    #[test]
    fn parse_timeframe_accepts_seconds_and_rejects_garbage() {
        // Regression: a "30s" series loads from the hist store, so it must also be a valid
        // backtest timeframe (this used to error before the interval vocab was unified).
        assert_eq!(parse_timeframe("30s").unwrap(), 30_000);
        assert_eq!(parse_timeframe("1m").unwrap(), 60_000);
        assert_eq!(parse_timeframe("4h").unwrap(), 14_400_000);
        assert!(parse_timeframe("1x").is_err());
        assert!(parse_timeframe("m").is_err());
    }
}
