//! OANDA candles → [`Bar`]. Data half of the co-located adapter (orders/fills land next).

use crate::rest::{OandaApiError, OandaRest};
use vike_model::Bar;

/// Map a canonical interval to OANDA's granularity code. `None` for an interval OANDA lacks.
pub fn granularity(interval: &str) -> Option<&'static str> {
    Some(match interval {
        "1m" => "M1",
        "2m" => "M2",
        "4m" => "M4",
        "5m" => "M5",
        "10m" => "M10",
        "15m" => "M15",
        "30m" => "M30",
        "1h" => "H1",
        "2h" => "H2",
        "3h" => "H3",
        "4h" => "H4",
        "6h" => "H6",
        "8h" => "H8",
        "12h" => "H12",
        "1d" => "D",
        "1w" => "W",
        "1mo" => "M",
        _ => return None,
    })
}

/// Canonical symbol → OANDA instrument form (`"eurusd"` -> `"EUR_USD"`).
pub fn to_oanda_instrument(symbol: &str) -> String {
    let up = symbol.to_uppercase();
    if up.contains('_') {
        return up;
    }
    if up.len() == 6 && up.chars().all(|c| c.is_ascii_alphabetic()) {
        format!("{}_{}", &up[..3], &up[3..])
    } else {
        up
    }
}

/// One OANDA candle object (`price=M` midpoint, UNIX datetime format) → [`Bar`], IGNORING its
/// `complete` flag — the shared extraction under [`parse_candles`] (complete-only, the lossless
/// lane) and [`crate::market_data::parse_forming_candle`] (the forming tail, the conflating
/// lane). `None` on a malformed candle (no `mid`, unparseable prices).
pub(crate) fn bar_from_candle(c: &serde_json::Value) -> Option<Bar> {
    let mid = c.get("mid")?;
    let px = |k: &str| mid.get(k).and_then(|s| s.as_str()).and_then(|s| s.parse::<f64>().ok());
    let (open, high, low, close) = (px("o")?, px("h")?, px("l")?, px("c")?);
    let ts = c
        .get("time")
        .and_then(|t| t.as_str())
        .and_then(|s| s.parse::<f64>().ok())
        .map_or(0, |secs| (secs * 1000.0) as i64);
    let volume = c.get("volume").and_then(serde_json::Value::as_f64).unwrap_or(0.0);
    Some(Bar {
        ts,
        open,
        high,
        low,
        close,
        volume,
        funding: None,
        bid: None,
        ask: None,
        symbol: None,
    })
}

/// Parse an OANDA `candles` response (UNIX datetime format, `price=M` midpoints) into bars.
/// Only `complete` candles are kept; prices are decimal STRINGS and `time` is an epoch-seconds
/// string (from the `Accept-Datetime-Format: UNIX` header).
pub fn parse_candles(v: &serde_json::Value) -> Vec<Bar> {
    let Some(candles) = v.get("candles").and_then(|c| c.as_array()) else {
        return Vec::new();
    };
    candles
        .iter()
        .filter(|c| c.get("complete").and_then(serde_json::Value::as_bool).unwrap_or(false))
        .filter_map(bar_from_candle)
        .collect()
}

/// Fetch the most recent `count` candles for `instrument` at `interval` (midpoint OHLC).
pub fn fetch_candles(
    rest: &OandaRest,
    base: &str,
    instrument: &str,
    interval: &str,
    count: u32,
) -> Result<Vec<Bar>, OandaApiError> {
    let g = granularity(interval).ok_or_else(|| OandaApiError {
        status: 0,
        message: format!("unsupported interval {interval}"),
    })?;
    let instr = to_oanda_instrument(instrument);
    let query = format!("granularity={g}&count={count}&price=M");
    let v = rest.get(base, &format!("/v3/instruments/{instr}/candles"), &query)?;
    Ok(parse_candles(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instrument_and_granularity() {
        assert_eq!(to_oanda_instrument("eurusd"), "EUR_USD");
        assert_eq!(to_oanda_instrument("EUR_USD"), "EUR_USD");
        assert_eq!(to_oanda_instrument("XAUUSD"), "XAU_USD");
        assert_eq!(granularity("1h"), Some("H1"));
        assert_eq!(granularity("1d"), Some("D"));
        assert_eq!(granularity("7s"), None);
    }

    #[test]
    fn parse_candles_keeps_only_complete() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
                "instrument": "EUR_USD",
                "granularity": "H1",
                "candles": [
                    {"complete": true,  "volume": 100, "time": "1478012400.000000000",
                     "mid": {"o": "1.09000", "h": "1.09500", "l": "1.08900", "c": "1.09300"}},
                    {"complete": false, "volume": 5,   "time": "1478016000.000000000",
                     "mid": {"o": "1.09300", "h": "1.09400", "l": "1.09200", "c": "1.09350"}}
                ]
            }"#,
        )
        .unwrap();
        let bars = parse_candles(&v);
        assert_eq!(bars.len(), 1); // the forming candle is dropped
        let b = &bars[0];
        assert_eq!(b.ts, 1_478_012_400_000);
        assert_eq!(b.open, 1.09);
        assert_eq!(b.high, 1.095);
        assert_eq!(b.close, 1.093);
        assert_eq!(b.volume, 100.0);
    }
}
